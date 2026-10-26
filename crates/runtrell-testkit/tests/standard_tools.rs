// SPDX-License-Identifier: Apache-2.0
use runtrell::{data::*, execution::*, file_tools::*, memory::MemoryStore, standard_tools::*, *};
use runtrell_testkit::*;
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

fn access(owner: RunHandle) -> Access {
    Access::owner(Namespace {
        owner,
        agent: "main".into(),
    })
}
#[test]
fn selection_disable_overrides_and_missing_resources() {
    let empty = ToolSet::<DataStore>::default().build().unwrap();
    assert!(empty.is_empty());
    let errors = ToolSet::<DataStore>::default()
        .select(StandardTool::Execution)
        .select(StandardTool::Read)
        .build()
        .err()
        .unwrap();
    assert_eq!(errors.len(), 2);
    assert!(errors.iter().any(|d| d.code=="EXECUTION_BACKEND_MISSING" && d.message.contains("ExecutionLedger")));
    let a = access(RunHandle {
        tenant: "t".into(),
        id: "r".into(),
    });
    let tools = ToolSet::default()
        .files(DataStore::memory(StoragePolicy::default()), a)
        .preset(ToolPreset(BTreeMap::from([
            (StandardTool::Read, ToolSettings::default()),
            (StandardTool::Search, ToolSettings::default()),
        ])))
        .disable(StandardTool::Search)
        .settings(
            StandardTool::Read,
            ToolSettings {
                timeout: Some(99),
                max_output_bytes: Some(512),
                ..Default::default()
            },
        )
        .build()
        .unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].descriptor().name, "files.read");
    assert_eq!(tools[0].descriptor().timeout, 99);
    assert_eq!(tools[0].descriptor().max_output_bytes, 512);
}
struct ReadOnly;
impl Policy for ReadOnly {
    fn authorize(&self, _: &RunRecord, _: &str, permission: &str) -> Result<()> {
        if permission == "workspace.read" {
            Ok(())
        } else {
            Err(Error::PermissionDenied)
        }
    }
}
#[test]
fn selected_catalog_is_filtered_by_run_policy() {
    block_on(async {
        let s = MemoryStore::default();
        let account = s
            .create_account("t", Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let h = s
            .start(RunSpec::new("t", "catalog", account, "decide"))
            .await
            .unwrap();
        let tools = ToolSet::default()
            .files(
                DataStore::memory(StoragePolicy::default()),
                access(h.clone()),
            )
            .select(StandardTool::Read)
            .select(StandardTool::Write)
            .build()
            .unwrap();
        let catalog = pin_catalog(&s.get(&h).await.unwrap(), &ReadOnly, &tools).unwrap();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name, "files.read");
    });
}
async fn setup() -> (Arc<MemoryStore>, Claim, DataStore, Access) {
    setup_deadline(None).await
}
async fn setup_deadline(deadline: Option<u64>) -> (Arc<MemoryStore>, Claim, DataStore, Access) {
    let s = Arc::new(MemoryStore::default());
    let account = s
        .create_account("t", Amount::new(10000, 10000), None)
        .await
        .unwrap();
    let mut spec = RunSpec::new("t", "execution-tool", account, "decide");
    spec.deadline = deadline;
    let h = s.start(spec).await.unwrap();
    let c = s.claim(&h, "worker", 0, 100000).await.unwrap();
    (s, c, DataStore::memory(StoragePolicy::default()), access(h))
}
fn tool(
    s: Arc<MemoryStore>,
    w: DataStore,
    a: Access,
    b: FakeExecutionBackend,
    l: MemoryExecutionLedger,
) -> ExecutionTool<
    MemoryStore,
    DataStore,
    MemoryExecutionLedger,
    FakeExecutionBackend,
    FakePolicy,
    TestClock,
> {
    ExecutionTool::new(
        s,
        w,
        a,
        l,
        b,
        (FakePolicy::default(), TestClock::default()),
        ExecutionToolConfig {
            executables: [("fake".into(), ArgumentPolicy::Any)].into_iter().collect(),
            limits: ExecutionLimits::default(),
            requires_descendants: false,
            reserve: Amount::new(0, 100),
            summary_bytes: 4,
        },
    )
}
fn input() -> serde_json::Value {
    json!({"program":{"Process":{"executable":"fake","args":[]}},"files":["input"],"outputs":["out"]})
}
#[test]
fn execution_model_call_copies_only_selected_inputs_saves_and_reclaims() {
    block_on(async {
        let (s, c, w, a) = setup().await;
        w.write(&a, "input", None, b"source").await.unwrap();
        w.write(&a, "private", None, b"hidden").await.unwrap();
        let b = FakeExecutionBackend::default();
        *b.output.lock().unwrap() = Some(
            json!({"stdout":b"123456".to_vec(),"stderr":[],"exit":0,"files":{"out":b"result".to_vec()}}),
        );
        let l = MemoryExecutionLedger::default();
        let t = tool(s.clone(), w.clone(), a.clone(), b, l.clone());
        let tools = ToolSet::<DataStore>::default()
            .select(StandardTool::Execution)
            .execution(t)
            .build()
            .unwrap();
        let model = FakeModel::scripted(vec![
            FakeModel::calls(vec![ToolCall {
                id: "execute".into(),
                name: "execute".into(),
                arguments: input(),
            }]),
            FakeModel::decision(json!({"decision":"keep"})),
        ]);
        let rt = Runtime {
            store: s.clone(),
            model,
            tools,
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        assert_eq!(
            rt.drive(&c).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        let op = s
            .list_operations(&c.run)
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.request.kind == OperationKind::Sandbox)
            .unwrap();
        assert!(op.settled);
        let record = l.get(&c.run, &op.id).await.unwrap();
        assert!(record.reclaimed());
        assert_eq!(record.request.input_files.len(), 1);
        assert!(!record.request.input_files.contains_key("private"));
        let Some(ExternalResult::Applied { output, .. }) = op.result else {
            panic!("missing output")
        };
        assert_eq!(output["saved"], true);
        assert_eq!(output["stdout"]["text"], "1234");
        assert_eq!(output["stdout"]["truncated"], true);
        assert!(output["artifact"]["hash"].is_string());
        let snap = w.snapshot(&a).await.unwrap();
        let out = snap.files.iter().find(|f| f.path == "out").unwrap();
        assert_eq!(
            w.read(&a, "out", out.revision, 0, 20).await.unwrap().bytes,
            b"result"
        );
        assert_eq!(s.slot_count(OperationKind::Sandbox).await, 0);
    });
}
fn runtime_with(
    t: impl Tool + 'static,
    s: Arc<MemoryStore>,
    clock: TestClock,
) -> Runtime<
    MemoryStore,
    FakeModel,
    runtrell::assembly::RegisteredTool,
    FakePolicy,
    TestClock,
    DecisionValidator,
    DecisionFinalizer,
> {
    Runtime {
        store: s,
        model: FakeModel::scripted(vec![
            FakeModel::calls(vec![ToolCall {
                id: "x".into(),
                name: "execute".into(),
                arguments: input(),
            }]),
            FakeModel::decision(json!({"decision":"keep"})),
        ]),
        tools: vec![runtrell::assembly::RegisteredTool::new(t)],
        policy: FakePolicy::default(),
        clock,
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    }
}
#[test]
fn execution_budget_deadline_and_cancel_prevent_acquisition() {
    block_on(async {
        for mode in 0..3 {
            let (s, c, w, a) = setup_deadline(if mode == 1 { Some(1) } else { None }).await;
            w.write(&a, "input", None, b"source").await.unwrap();
            let l = MemoryExecutionLedger::default();
            let mut t = tool(s.clone(), w, a, FakeExecutionBackend::default(), l.clone());
            if mode == 0 {
                t.config.reserve = Amount::new(0, 20000);
            }
            let clock = t.clock.clone();
            let rt = runtime_with(t, s.clone(), clock.clone());
            if mode == 1 {
                clock.set(1);
            }
            if mode == 2 {
                s.cancel(&c.run, "cancel before execution").await.unwrap();
            }
            let result = rt.drive(&c).await;
            if mode == 0 {
                assert!(matches!(
                    result.unwrap().state,
                    RunState::Terminal(Terminal::BudgetReached(_))
                ));
            } else if mode == 1 {
                assert_eq!(
                    result.unwrap().state,
                    RunState::Terminal(Terminal::DeadlineReached)
                );
            } else {
                assert_ne!(
                    result.unwrap().state,
                    RunState::Terminal(Terminal::Succeeded)
                );
            }
            assert!(l.pending().await.unwrap().is_empty());
            assert_eq!(s.slot_count(OperationKind::Sandbox).await, 0);
        }
    });
}
#[test]
fn execution_unknown_cost_retains_reservation_and_cancel_reclaims() {
    block_on(async {
        use std::sync::atomic::Ordering;
        let (s, c, w, a) = setup().await;
        w.write(&a, "input", None, b"source").await.unwrap();
        let b = FakeExecutionBackend::default();
        b.unknown_cost.store(true, Ordering::SeqCst);
        let l = MemoryExecutionLedger::default();
        let rt = runtime_with(
            tool(s.clone(), w, a, b.clone(), l.clone()),
            s.clone(),
            TestClock::default(),
        );
        assert!(matches!(
            rt.drive(&c).await.unwrap().state,
            RunState::Waiting(_)
        ));
        let pending = l.pending().await.unwrap();
        assert_eq!(pending.len(), 1);
        assert!(!pending[0].resources[&Resource::ExternalCost].released);
        assert_eq!(s.slot_count(OperationKind::Sandbox).await, 1);
        b.unknown_cost.store(false, Ordering::SeqCst);
        rt.cancel(&c, "stop").await.unwrap();
        assert!(l.pending().await.unwrap().is_empty());
        assert_eq!(s.slot_count(OperationKind::Sandbox).await, 0);
    });
}
#[test]
fn execution_rejects_undeclared_outputs_without_overwriting_workspace() {
    block_on(async {
        let (s, c, w, a) = setup().await;
        w.write(&a, "input", None, b"source").await.unwrap();
        let b = FakeExecutionBackend::default();
        *b.output.lock().unwrap() =
            Some(json!({"stdout":[],"stderr":[],"exit":0,"files":{"input":b"bad".to_vec()}}));
        let rt = runtime_with(
            tool(
                s.clone(),
                w.clone(),
                a.clone(),
                b,
                MemoryExecutionLedger::default(),
            ),
            s.clone(),
            TestClock::default(),
        );
        rt.drive(&c).await.unwrap();
        let snap = w.snapshot(&a).await.unwrap();
        assert_eq!(
            w.read(&a, "input", snap.files[0].revision, 0, 20)
                .await
                .unwrap()
                .bytes,
            b"source"
        );
        let op = s
            .list_operations(&c.run)
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.request.kind == OperationKind::Sandbox)
            .unwrap();
        let Some(ExternalResult::Applied { output, .. }) = op.result else {
            panic!()
        };
        assert_eq!(output["saved"], false);
        assert!(output["artifact"].is_object());
    });
}

#[test]
fn cancellation_during_acquisition_never_starts_payload() {
    use std::sync::atomic::Ordering;
    block_on(async {
        for deadline in [false, true] {
            let (s, c, w, a) = setup_deadline(if deadline { Some(10) } else { None }).await;
            w.write(&a, "input", None, b"source").await.unwrap();
            let b = FakeExecutionBackend::default();
            b.delay.store(true, Ordering::SeqCst);
            let l = MemoryExecutionLedger::default();
            let t = tool(s.clone(), w, a, b.clone(), l.clone());
            let clock = t.clock.clone();
            let rt = Arc::new(runtime_with(t, s.clone(), clock.clone()));
            let worker = rt.clone();
            let claim = c.clone();
            let handle = std::thread::spawn(move || block_on(worker.drive(&claim)));
            let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let pending = l.pending().await.unwrap();
                if let Some(row) = pending.first() {
                    let ctx = ToolContext {
                        recovery_proof: None,
                        claim: c.clone(),
                        principal: "host".into(),
                        operation: row.context_operation.clone(),
                        attempt: 1,
                        deadline: None,
                        permission: "execute".into(),
                        account_binding: "execution".into(),
                    };
                    if b.lookup(&ctx).await.unwrap().is_some() {
                        break;
                    }
                }
                assert!(
                    std::time::Instant::now() < until,
                    "acquisition did not occur"
                );
                std::thread::yield_now();
            }
            if deadline {
                clock.set(10);
            } else {
                rt.cancel(&c, "during acquisition").await.unwrap();
            }
            b.delay.store(false, Ordering::SeqCst);
            let result = handle.join().unwrap();
            if deadline {
                assert!(matches!(result.unwrap().state, RunState::Waiting(_)));
            }
            rt.reconcile(&c).await.unwrap();
            if deadline {
                assert_eq!(
                    rt.drive(&c).await.unwrap().state,
                    RunState::Terminal(Terminal::DeadlineReached)
                );
            }
            let ops = s.list_operations(&c.run).await.unwrap();
            let op = ops
                .iter()
                .find(|o| o.request.kind == OperationKind::Sandbox)
                .unwrap();
            let row = l.get(&c.run, &op.id).await.unwrap();
            assert!(row.cancel_requested);
            assert!(row.reclaimed());
            assert!(
                row.observation.unwrap().result.is_none(),
                "payload must not start"
            );
            assert_eq!(s.slot_count(OperationKind::Sandbox).await, 0);
        }
    });
}

#[test]
fn save_conflict_preserves_newer_workspace_and_artifact() {
    block_on(async {
        use std::sync::atomic::Ordering;
        let (s, c, w, a) = setup().await;
        w.write(&a, "input", None, b"source").await.unwrap();
        let b = FakeExecutionBackend::default();
        *b.output.lock().unwrap() =
            Some(json!({"stdout":[],"stderr":[],"exit":0,"files":{"out":b"result".to_vec()}}));
        b.unknown_cost.store(true, Ordering::SeqCst);
        let l = MemoryExecutionLedger::default();
        let rt = runtime_with(
            tool(s.clone(), w.clone(), a.clone(), b.clone(), l.clone()),
            s.clone(),
            TestClock::default(),
        );
        assert!(matches!(
            rt.drive(&c).await.unwrap().state,
            RunState::Waiting(_)
        ));
        let current = w.write(&a, "out", None, b"newer").await.unwrap();
        b.unknown_cost.store(false, Ordering::SeqCst);
        rt.reconcile(&c).await.unwrap();
        let op = s
            .list_operations(&c.run)
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.request.kind == OperationKind::Sandbox)
            .unwrap();
        let Some(ExternalResult::Applied { output, .. }) = op.result else {
            panic!()
        };
        assert_eq!(output["saved"], false);
        assert_eq!(output["save_error"], Error::Conflict.to_string());
        assert_eq!(
            w.read(&a, "out", current.revision, 0, 20)
                .await
                .unwrap()
                .bytes,
            b"newer"
        );
        let artifact: ArtifactRef = serde_json::from_value(output["artifact"].clone()).unwrap();
        assert!(w.read_artifact(&a, &artifact, 0, 1000).await.unwrap().total > 0);
        assert!(l.pending().await.unwrap().is_empty());
        assert!(op.settled);
    });
}

#[test]
fn missing_input_settles_zero_without_creating_execution_intent() {
    block_on(async {
        let (s, c, w, a) = setup().await;
        let l = MemoryExecutionLedger::default();
        let t = tool(s.clone(), w, a, FakeExecutionBackend::default(), l.clone());
        let rt = runtime_with(t, s.clone(), TestClock::default());
        assert!(matches!(
            rt.drive(&c).await.unwrap().state,
            RunState::Terminal(Terminal::Failed(_))
        ));
        let op = s
            .list_operations(&c.run)
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.request.kind == OperationKind::Sandbox)
            .unwrap();
        assert!(op.settled);
        assert!(matches!(op.result, Some(ExternalResult::Failed { .. })));
        assert!(l.pending().await.unwrap().is_empty());
        assert_eq!(s.slot_count(OperationKind::Sandbox).await, 0);
    });
}

#[test]
fn program_definition_declares_only_configured_capabilities() {
    block_on(async {
        let (s, c, w, a) = setup().await;
        let t = tool(
            s,
            w,
            a,
            FakeExecutionBackend::default(),
            MemoryExecutionLedger::default(),
        );
        let schema = t.descriptor().input_schema;
        assert!(schema.to_string().contains("Process"));
        assert!(!schema.to_string().contains("Python"));
        let mut denied = input();
        denied["program"]["Process"]["executable"] = json!("not-allowed");
        assert!(!schema_diagnostics(&denied, &schema).unwrap().is_empty());
        assert!(schema_diagnostics(&input(), &schema).unwrap().is_empty());
        assert_eq!(t.access.actor().owner, c.run);
    });
}

#[cfg(feature = "python")]
#[test]
fn python_tool_uses_selected_script_and_pinned_runtime_only_when_configured() {
    block_on(async {
        let (s, c, w, a) = setup().await;
        w.write(&a, "job.py", None, b"print(42)").await.unwrap();
        let runtime_path = std::env::temp_dir().join(format!("runtrell-test-runtime-{}", c.run.id));
        std::fs::write(&runtime_path, b"synthetic-host-runtime").unwrap();
        let python = python::PythonRuntime::new(
            runtime_path.to_str().unwrap(),
            "3.12.3",
            &content_digest(b"synthetic-host-runtime"),
        )
        .unwrap();
        let b = FakeExecutionBackend::default();
        *b.output.lock().unwrap() =
            Some(json!({"stdout":b"42".to_vec(),"stderr":[],"exit":0,"files":{}}));
        let l = MemoryExecutionLedger::default();
        let mut t = tool(s.clone(), w, a, b, l.clone()).python(python);
        t.config.executables.clear();
        assert!(t.descriptor().input_schema.to_string().contains("Python"));
        assert!(!t.descriptor().input_schema.to_string().contains("Process"));
        let tools = ToolSet::<DataStore>::default()
            .select(StandardTool::Execution)
            .execution(t)
            .build()
            .unwrap();
        let mut rt = runtime_with(BusinessTool::new("unused"), s.clone(), TestClock::default());
        rt.tools = tools;
        rt.model = FakeModel::scripted(vec![
            FakeModel::calls(vec![ToolCall {
                id: "script".into(),
                name: "execute".into(),
                arguments: json!({"program":{"Python":{"script":"job.py"}},"files":["job.py"],"outputs":[]}),
            }]),
            FakeModel::decision(json!({"decision":"keep"})),
        ]);
        assert_eq!(
            rt.drive(&c).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        let op = s
            .list_operations(&c.run)
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.request.kind == OperationKind::Sandbox)
            .unwrap();
        let row = l.get(&c.run, &op.id).await.unwrap();
        assert_eq!(
            row.request.executable_hash,
            Some(content_digest(b"synthetic-host-runtime"))
        );
        assert_eq!(row.request.input_files["job.py"], b"print(42)");
        assert!(row.request.limits.network_allow.is_empty());
        let Program::Process { args, .. } = &row.request.program;
        assert_eq!(&args[..2], ["-I", "-B"]);
        assert!(row.reclaimed());
        std::fs::remove_file(runtime_path).unwrap();
    });
}

#[test]
fn selected_execution_reports_capability_diagnostics_but_disabled_does_not() {
    block_on(async {
        for selected in [true, false] {
            let (s, _c, w, a) = setup().await;
            let mut t = tool(
                s,
                w,
                a,
                FakeExecutionBackend::default(),
                MemoryExecutionLedger::default(),
            );
            t.config.limits.ram_bytes = Some(1024);
            let builder = ToolSet::<DataStore>::default()
                .select(StandardTool::Execution)
                .execution(t);
            if selected {
                let diagnostics = builder.build().err().unwrap();
                assert!(
                    diagnostics
                        .iter()
                        .any(|d| d.code == "EXECUTION_CAPABILITY_MISSING")
                );
            } else {
                assert!(
                    builder
                        .disable(StandardTool::Execution)
                        .build()
                        .unwrap()
                        .is_empty()
                );
            }
        }
    });
}

#[test]
fn argument_policy_is_pinned_in_definition_and_enforced_per_call() {
    block_on(async {
        let (s, c, w, a) = setup().await;
        w.write(&a, "input", None, b"source").await.unwrap();
        let l = MemoryExecutionLedger::default();
        let mut t = tool(s, w, a, FakeExecutionBackend::default(), l.clone());
        let before = catalog_digest(&t.descriptor());
        t.config.executables.insert(
            "fake".into(),
            ArgumentPolicy::Exact([vec![]].into_iter().collect()),
        );
        assert_ne!(before, catalog_digest(&t.descriptor()));
        let mut denied = input();
        denied["program"]["Process"]["args"] = json!(["forbidden"]);
        assert!(
            !schema_diagnostics(&denied, &t.descriptor().input_schema)
                .unwrap()
                .is_empty()
        );
        let ctx = ToolContext {
            recovery_proof: None,
            claim: c,
            principal: "host".into(),
            operation: "args".into(),
            attempt: 1,
            deadline: None,
            permission: "execute".into(),
            account_binding: "execution".into(),
        };
        assert!(matches!(
            t.invoke(&ctx, denied).await.unwrap(),
            ExternalResult::Failed { .. }
        ));
        assert!(l.pending().await.unwrap().is_empty());
    });
}
