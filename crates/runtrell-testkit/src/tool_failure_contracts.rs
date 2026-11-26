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
        match self.mode {
            "invoke_permission" => return Err(Error::PermissionDenied),
            "invoke_invalid" => {
                return Err(Error::InvalidToolInput {
                    path: "/value".into(),
                    hint: "Provide a valid value.".into(),
                });
            }
            "pre_send" => {
                self.clock.set(233);
                return Ok(ExternalResult::retryable_not_sent());
            }
            _ => {}
        }
        if self.mode == "unknown" {
            return Ok(crate::ambiguous_fixture());
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
            provider_diagnostic: None,
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
        "invoke_permission",
        "invoke_invalid",
        "pre_send",
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
        let claim = store.claim(&run, "worker", 0, 1_000).await.unwrap();
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
            "invoke_invalid" | "pre_send" => {
                assert_eq!(done.state, RunState::Terminal(Terminal::Succeeded))
            }
            "deadline" => assert_eq!(done.state, RunState::Terminal(Terminal::DeadlineReached)),
            _ => assert!(matches!(
                done.state,
                RunState::Terminal(Terminal::Failed(_))
            )),
        }
        let answered = matches!(mode, "invoke_invalid" | "pre_send");
        {
            let requests = model.requests.lock().unwrap();
            assert_eq!(requests.len(), if answered { 2 } else { 1 }, "{mode}");
            if answered {
                let response = requests[1]
                    .messages
                    .iter()
                    .find(|message| message.role == "tool")
                    .unwrap();
                if mode == "pre_send" {
                    assert_eq!(response.data["retryable"], true);
                } else {
                    assert_eq!(response.data["code"], "TOOL_INPUT_INVALID");
                }
            } else {
                assert!(
                    done.checkpoint.unwrap().payload["tool_repair"].is_null(),
                    "{mode}"
                );
            }
        }
        let expected = if mode == "unknown" {
            vec![outcome_contracts::OutcomeException::unresolved(
                "tool:1:failure",
                OperationKind::Tool,
                AmbiguityReason::InvocationInterrupted,
            )]
        } else {
            vec![]
        };
        outcome_contracts::assert_no_unexpected_ambiguity(store.as_ref(), &run, &expected, false)
            .await;
        if !expected.is_empty() {
            runtrell::test_outcomes::expect(
                &run,
                runtrell::test_outcomes::ExpectedOutcome {
                    exceptions: expected,
                    ..Default::default()
                },
            );
        }
    }
}

/// Host dispatch returns the settled receipt without producing a model tool answer.
pub async fn host_tool_settlement<S: ControlStore>(store: Arc<S>) {
    let measured = Usage::measured(2, 3, 7);
    for (key, result, used, terminal) in [
        (
            "before-send",
            ExternalResult::retryable_not_sent(),
            Amount::default(),
            Terminal::Failed(Error::Invalid("REQUEST_NOT_SENT".into()).to_string()),
        ),
        (
            "before-send-with-next-phase",
            ExternalResult::retryable_not_sent(),
            Amount::default(),
            Terminal::Failed(Error::Invalid("REQUEST_NOT_SENT".into()).to_string()),
        ),
        (
            "sent-and-applied",
            ExternalResult::Applied {
                physical_evidence: None,
                output: json!({"value":1}),
                usage: measured.clone(),
                external_id: Some("receipt".into()),
            },
            Amount::new(5, 7),
            Terminal::Succeeded,
        ),
        (
            "sent-and-failed",
            ExternalResult::Failed {
                physical_evidence: None,
                code: "REMOTE_FAILURE".into(),
                usage: measured,
                retry_after: None,
                repair_hint: None,
                provider_diagnostic: Some(ProviderDiagnostic {
                    phase: ProviderFailurePhase::HttpError,
                    send_certainty: ProviderSendCertainty::Sent,
                    http_status: Some(500),
                    provider_type: None,
                    provider_code: None,
                    provider_param: None,
                    request_id: None,
                }),
            },
            Amount::new(5, 7),
            Terminal::Failed(Error::Invalid("REMOTE_FAILURE".into()).to_string()),
        ),
    ] {
        let account = store
            .create_account("t", Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let tool = BusinessTool::new("host-tool");
        let descriptor = tool.descriptor();
        let mut spec = RunSpec::new("t", key, account.clone(), "act");
        if key == "before-send-with-next-phase" {
            spec.phases = ["prepare", "finish"]
                .into_iter()
                .map(|id| PhaseConfig {
                    id: id.into(),
                    tools: [descriptor.name.clone()].into(),
                    completion: CompletionPolicy::default(),
                })
                .collect();
        }
        let run = store.start(spec).await.unwrap();
        let claim = store.claim(&run, "host", 0, 100).await.unwrap();
        let rt = Runtime {
            store: store.clone(),
            model: FakeModel::scripted(vec![FakeModel::decision(json!({"decision":"keep"}))]),
            tools: vec![tool],
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let mut request = DispatchRequest::new(
            "host-call",
            OperationKind::Tool,
            &descriptor.name,
            json!({}),
            Amount::new(10, 10),
        );
        request.version = descriptor.version.clone();
        request.scope = Some(catalog_digest(&descriptor));
        request.account_binding = descriptor.account.clone();
        let first = rt
            .dispatch(
                &claim,
                request.clone(),
                &descriptor.permission,
                |_| async { Ok(result.clone()) },
                |_| async { panic!("a fresh call must not look up a receipt") },
            )
            .await
            .unwrap();
        assert_eq!(first, result);
        let replay = rt
            .dispatch(
                &claim,
                request,
                &descriptor.permission,
                |_| async { panic!("a settled call must not execute again") },
                |_| async { panic!("a settled call must not look up a receipt") },
            )
            .await
            .unwrap();
        assert_eq!(replay, first);
        let op = store.operation(&run, "host-call").await.unwrap().unwrap();
        assert!(op.settled);
        assert_eq!(op.usage_outcome(), first.usage_outcome());
        assert_eq!(op.send_count, 1);
        let balance = store.account(&account).await.unwrap();
        assert_eq!(balance.used, used);
        assert_eq!(balance.reserved, Amount::default());
        assert_eq!(balance.unknown, 0);
        assert_eq!(
            rt.drive(&claim).await.unwrap().state,
            RunState::Terminal(terminal),
            "{key}"
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
    // The completion guard preserves the known failure until model delivery.
    assert_eq!(
        reopened.finalize(&claim, completion, 0).await.unwrap_err(),
        Error::Invalid("MULTIPLE_MATCHES".into())
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

/// Four distinct rejected edits, interleaved with reads, can recover under a
/// validation policy that permits only three repairs.
pub async fn exact_replace_recovery_trace<S: ControlStore>(store: Arc<S>) {
    let account = store
        .create_account("t", Amount::new(1000, 1000), None)
        .await
        .unwrap();
    let run = store
        .start(RunSpec::new("t", "edit-trace", account, "revise a section"))
        .await
        .unwrap();
    let claim = store.claim(&run, "worker", 0, 100).await.unwrap();
    let data = DataStore::memory(StoragePolicy::default());
    let access = Access::owner(Namespace {
        owner: run.clone(),
        agent: "main".into(),
    });
    let file = data
        .write(&access, "a.md", None, b"one two one")
        .await
        .unwrap();
    data.write(&access, "b.md", None, b"unrelated")
        .await
        .unwrap();
    let call = |id: &str, name: &str, arguments| {
        FakeModel::calls(vec![ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        }])
    };
    let edit = |id, revision, digest: &str, old: &str| {
        call(
            id,
            "files.exact_replace",
            json!({
                "expected_revision": revision,
                "edits":[{"path":"a.md","expected_digest":digest,"old_text":old,"new_text":"three"}]
            }),
        )
    };
    let model = FakeModel::scripted(vec![
        edit("ambiguous", 2, &file.hash, "one"),
        edit("absent", 2, &file.hash, "missing"),
        call(
            "read",
            "files.read",
            json!({"path":"a.md","revision":1,"offset":0,"length":32}),
        ),
        edit("stale-digest", 2, "stale", "one two"),
        call("list", "files.list", json!({"limit":10})),
        // A listed file revision is not the current workspace revision.
        edit("stale-revision", 1, &file.hash, "one two"),
        edit("fixed", 2, &file.hash, "one two"),
        FakeModel::decision(json!({"decision":"keep"})),
    ]);
    let rt = Runtime {
        store: store.clone(),
        model: model.clone(),
        tools: [FileAction::ExactReplace, FileAction::Read, FileAction::List]
            .into_iter()
            .map(|action| FileTool {
                store: data.clone(),
                access: access.clone(),
                action,
            })
            .collect(),
        policy: FakePolicy::default(),
        clock: TestClock::default(),
        validator: Arc::new(runtrell::assembly::ValidationChain {
            validators: vec![],
            schema: json!({}),
            repair: RepairPolicy {
                max_repairs: Some(3),
                ..RepairPolicy::default()
            },
        }),
        finalizer: Arc::new(DecisionFinalizer),
    };
    let done = rt.drive(&claim).await.unwrap();
    assert_eq!(done.state, RunState::Terminal(Terminal::Succeeded));
    let cp = done.checkpoint.as_ref().unwrap();
    assert_eq!(cp.payload["tool_repair"]["count"], 4);
    assert!(cp.payload["repair"].is_null());
    assert_eq!(
        data.read(&access, "a.md", 3, 0, 32).await.unwrap().bytes,
        b"three one"
    );
    {
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 8);
        for (turn, id, code) in [
            (1, "ambiguous", "MULTIPLE_MATCHES"),
            (2, "absent", "NO_MATCH"),
            (4, "stale-digest", "CONFLICT"),
            (6, "stale-revision", "CONFLICT"),
        ] {
            let feedback = &requests[turn]
                .messages
                .iter()
                .find(|message| message.call_id.as_deref() == Some(id) && message.role == "tool")
                .unwrap()
                .data;
            assert_eq!(feedback["code"], code);
            let hint = feedback["hint"].as_str().unwrap();
            assert!(
                !hint.contains("missing") && !hint.contains("one two") && !hint.contains("b.md")
            );
            match code {
                "MULTIPLE_MATCHES" => {
                    assert!(hint.contains("2 matches") && hint.contains("surrounding context"))
                }
                "NO_MATCH" => assert!(hint.contains("files.read") && hint.contains("whitespace")),
                "CONFLICT" => {
                    assert!(hint.contains("expected_revision=2"), "{hint}");
                    assert!(
                        hint.contains(&format!("expected_digest={}", file.hash)),
                        "{hint}"
                    );
                    assert!(hint.contains("file revision 1"), "{hint}");
                }
                _ => unreachable!(),
            }
        }
    }
    let operations = store.list_operations(&run).await.unwrap();
    let failures: Vec<_> = operations
        .iter()
        .filter(|op| op.state == OperationState::Failed)
        .collect();
    assert_eq!(failures.len(), 4);
    assert!(
        failures
            .iter()
            .all(|op| op.send_count == 1 && op.model_repair_acknowledged(&done))
    );
}

pub async fn repair_stops<S: ControlStore>(store: Arc<S>) {
    for mode in [
        "validation_limits_are_independent",
        "correction_limit",
        "budget",
        "budget_disabled",
        "input_limit",
        "no_progress",
        "no_progress_schema",
        "no_progress_cycle",
        "no_progress_disabled",
        "no_progress_disabled_schema",
        "no_progress_disabled_nonobject",
    ] {
        let account = store
            .create_account(
                "t",
                Amount::new(
                    if matches!(mode, "budget" | "budget_disabled") {
                        21
                    } else {
                        1000
                    },
                    1000,
                ),
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
            "no_progress_schema" | "no_progress_disabled_schema" => {
                json!({"expected_revision":"bad","edits":[]})
            }
            "no_progress_disabled_nonobject" => Value::Null,
            _ => bad.clone(),
        };
        let call = |id: &str, arguments| {
            FakeModel::calls(vec![ToolCall {
                id: id.into(),
                name: "files.exact_replace".into(),
                arguments,
            }])
        };
        let repeated = if matches!(
            mode,
            "no_progress_schema" | "no_progress_disabled_schema" | "no_progress_disabled_nonobject"
        ) {
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
        if mode.starts_with("no_progress_disabled") {
            replies = (0..5)
                .map(|n| call(&format!("bad-{n}"), repeated.clone()))
                .collect();
            replies.push(FakeModel::decision(json!({"decision":"keep"})));
        }
        if mode == "correction_limit" {
            replies = (0..9)
                .map(|n| {
                    let mut invalid = repeated.clone();
                    invalid["edits"][0]["new_text"] = json!(format!("replacement {n}"));
                    call(&format!("bad-{n}"), invalid)
                })
                .collect();
        }
        let model = FakeModel::scripted(replies);
        let validator = runtrell::assembly::ValidationChain {
            validators: vec![],
            schema: json!({}),
            repair: RepairPolicy {
                max_repairs: (mode == "validation_limits_are_independent").then_some(0),
                max_elapsed: (mode == "validation_limits_are_independent").then_some(0),
                max_no_progress: if mode.starts_with("no_progress_disabled")
                    || matches!(
                        mode,
                        "budget_disabled" | "validation_limits_are_independent"
                    ) {
                    None
                } else {
                    RepairPolicy::default().max_no_progress
                },
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
            "no_progress_disabled"
            | "no_progress_disabled_schema"
            | "no_progress_disabled_nonobject"
            | "validation_limits_are_independent" => {
                assert_eq!(done.state, RunState::Terminal(Terminal::Succeeded))
            }
            "no_progress" | "no_progress_schema" | "no_progress_cycle" => assert!(matches!(
                done.state,
                RunState::Terminal(Terminal::Stalled(ProgressStop::Validation { .. }))
            )),
            "budget" | "budget_disabled" => assert!(matches!(
                done.state,
                RunState::Terminal(Terminal::BudgetReached(_))
            )),
            "input_limit" => assert!(
                matches!(done.state, RunState::Terminal(Terminal::Failed(ref code)) if code.contains("INPUT_LIMIT"))
            ),
            "correction_limit" => assert_eq!(
                done.state,
                RunState::Terminal(Terminal::Failed(
                    "invalid input: TOOL_INPUT_CORRECTION_LIMIT".into()
                ))
            ),
            _ => unreachable!(),
        }
        assert_eq!(
            model.requests.lock().unwrap().len(),
            if mode.starts_with("no_progress_disabled") {
                6
            } else if mode == "no_progress_cycle" {
                5
            } else if mode.starts_with("no_progress") {
                3
            } else if mode == "correction_limit" {
                9
            } else if mode == "validation_limits_are_independent" {
                4
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
            if mode.starts_with("no_progress_disabled") {
                5
            } else if mode == "no_progress_cycle" {
                4
            } else if mode.starts_with("no_progress") {
                2
            } else if mode == "correction_limit" {
                8
            } else if mode == "validation_limits_are_independent" {
                3
            } else if matches!(mode, "budget" | "budget_disabled") {
                1
            } else {
                0
            }
        );
    }
}

struct ScopedInput {
    quotes: Arc<AtomicUsize>,
    invocations: Arc<AtomicUsize>,
}

impl Tool for ScopedInput {
    fn descriptor(&self) -> ToolDescriptor {
        BusinessTool::new("scoped_read").descriptor()
    }

    async fn authorize_input(&self, _: &RunRecord, input: &Value) -> Result<()> {
        match input["path"].as_str() {
            Some("visible.txt") => Ok(()),
            Some("blocked.txt") => Err(Error::InvalidToolInput {
                path: "/path".into(),
                hint: "Choose a path in the current tool scope.".into(),
            }),
            _ => Err(Error::PermissionDenied),
        }
    }

    async fn quote(&self, _: &RunRecord, input: &Value) -> Result<Amount> {
        assert_eq!(input["path"], "visible.txt");
        self.quotes.fetch_add(1, Ordering::SeqCst);
        Ok(Amount::new(10, 10))
    }

    async fn invoke(&self, _: &ToolContext, input: Value) -> Result<ExternalResult> {
        assert_eq!(input["path"], "visible.txt");
        self.invocations.fetch_add(1, Ordering::SeqCst);
        Ok(ExternalResult::Applied {
            physical_evidence: None,
            output: json!({"found":true}),
            usage: Usage::measured(1, 1, 2),
            external_id: None,
        })
    }

    async fn lookup(&self, _: &ToolContext, _: Value) -> Result<Lookup> {
        panic!("input rejection must not trigger lookup")
    }
}

pub async fn correctable_input_authorization<S: ControlStore>(store: Arc<S>) {
    for correctable in [true, false] {
        let account = store
            .create_account("t", Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let run = store
            .start(RunSpec::new(
                "t",
                &format!("scoped-input-{correctable}"),
                account.clone(),
                "read a file",
            ))
            .await
            .unwrap();
        let claim = store.claim(&run, "worker", 0, 100).await.unwrap();
        let quotes = Arc::new(AtomicUsize::new(0));
        let invocations = Arc::new(AtomicUsize::new(0));
        let call = |id: &str, path: &str| {
            FakeModel::calls(vec![ToolCall {
                id: id.into(),
                name: "scoped_read".into(),
                arguments: json!({"path":path}),
            }])
        };
        let model = FakeModel::scripted(vec![
            call(
                "reject",
                if correctable {
                    "blocked.txt"
                } else {
                    "revoked"
                },
            ),
            call("accept", "visible.txt"),
            FakeModel::decision(json!({"decision":"keep"})),
        ]);
        let rt = Runtime {
            store: store.clone(),
            model: model.clone(),
            tools: vec![ScopedInput {
                quotes: quotes.clone(),
                invocations: invocations.clone(),
            }],
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let done = rt.drive(&claim).await.unwrap();
        let cp = done.checkpoint.unwrap();
        if correctable {
            let requests = model.requests.lock().unwrap();
            assert_eq!(done.state, RunState::Terminal(Terminal::Succeeded));
            assert_eq!(cp.cursor, 5);
            assert_eq!(cp.payload["tool_repair"]["count"], 1);
            let feedback = requests[1]
                .messages
                .iter()
                .find(|m| m.role == "tool")
                .unwrap();
            assert_eq!(feedback.call_id.as_deref(), Some("reject"));
            assert_eq!(feedback.data["code"], "TOOL_INPUT_INVALID");
            assert_eq!(feedback.data["diagnostics"][0]["path"], "/path");
            assert!(!feedback.data.to_string().contains("blocked.txt"));
        } else {
            assert_eq!(
                done.state,
                RunState::Terminal(Terminal::Failed("permission denied".into()))
            );
            assert_eq!(cp.cursor, 1);
            assert_eq!(model.requests.lock().unwrap().len(), 1);
            assert!(cp.messages.iter().all(|m| m.role != "tool"));
        }
        let expected = usize::from(correctable);
        assert_eq!(quotes.load(Ordering::SeqCst), expected);
        assert_eq!(invocations.load(Ordering::SeqCst), expected);
        let operations = store.list_operations(&run).await.unwrap();
        let tool_ops: Vec<_> = operations
            .iter()
            .filter(|op| op.request.kind == OperationKind::Tool)
            .collect();
        assert_eq!(tool_ops.len(), expected);
        assert!(
            tool_ops
                .iter()
                .all(|op| op.request.input["path"] == "visible.txt" && op.settled)
        );
        assert_eq!(
            store.account(&account).await.unwrap().reserved,
            Amount::default()
        );
    }
}
