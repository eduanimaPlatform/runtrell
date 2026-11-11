// SPDX-License-Identifier: Apache-2.0
use runtrell::{data::*, file_tools::*, memory::MemoryStore, *};
use runtrell_testkit::*;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
#[test]
fn file_execution_failures_are_model_repairable() {
    block_on(tool_failure_contracts::file_repairs(Arc::new(
        MemoryStore::default(),
    )));
}
#[test]
fn saved_file_failure_recovers_without_reexecution() {
    let store = Arc::new(MemoryStore::default());
    block_on(tool_failure_contracts::saved_failure_recovery(
        store.clone(),
        store,
    ));
}
#[test]
fn file_repair_preserves_limits_and_budget() {
    block_on(tool_failure_contracts::repair_stops(Arc::new(
        MemoryStore::default(),
    )));
}
#[test]
fn tool_failures_preserve_stop_and_reconciliation_boundaries() {
    block_on(tool_failure_contracts::failure_boundaries(Arc::new(
        MemoryStore::default(),
    )));
}
async fn start(s: &MemoryStore) -> (RunHandle, Claim) {
    let account = s
        .create_account("tenant", Amount::new(10000, 10000), None)
        .await
        .unwrap();
    let h = s
        .start(RunSpec::new("tenant", "data", account, "judge"))
        .await
        .unwrap();
    let c = s.claim(&h, "worker", 0, 100).await.unwrap();
    (h, c)
}
#[test]
fn standard_file_tools_use_dispatch_no_vm_and_current_policy() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (h, c) = start(&s).await;
        let data = DataStore::memory(StoragePolicy::default());
        let access = Access::owner(Namespace {
            owner: h,
            agent: "main".into(),
        });
        let tool = FileTool {
            store: data.clone(),
            access: access.clone(),
            action: FileAction::Write,
        };
        let rt = Runtime {
            store: s.clone(),
            model: FakeModel::scripted(vec![
                FakeModel::calls(vec![ToolCall {
                    id: "write".into(),
                    name: "files.write".into(),
                    arguments: json!({"path":"a","expected_revision":null,"text":"saved"}),
                }]),
                FakeModel::decision(json!({"decision":"keep"})),
            ]),
            tools: vec![tool.clone()],
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: std::sync::Arc::new(DecisionValidator::default()),
            finalizer: std::sync::Arc::new(DecisionFinalizer),
        };
        assert_eq!(
            rt.drive(&c).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        assert_eq!(data.snapshot(&access).await.unwrap().files.len(), 1);
        assert_eq!(s.slot_count(OperationKind::Sandbox).await, 0);
        let ops = s.list_operations(&c.run).await.unwrap();
        let op = ops
            .iter()
            .find(|o| o.request.kind == OperationKind::Tool)
            .unwrap();
        assert_eq!(op.request.scope, Some(catalog_digest(&tool.descriptor())));
        assert!(op.settled);
    });
}
#[test]
fn compaction_is_charged_and_preserves_contract() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (h, c) = start(&s).await;
        let data = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(Namespace {
            owner: h,
            agent: "main".into(),
        });
        data.context_contract(
            &a,
            0,
            "resolve case".into(),
            vec!["no reply".into()],
            vec!["lookup".into()],
            vec![],
        )
        .await
        .unwrap();
        data.append_context(
            &a,
            ContextItem {
                source: Source::Document,
                value: json!("evidence"),
            },
        )
        .await
        .unwrap();
        let model = FakeModel::scripted(vec![ModelReply {
            text: "Current findings".into(),
            calls: vec![],
            complete: true,
            metadata: Value::Null,
        }]);
        let rt = runtime(s.clone(), model, vec![]);
        let result = rt
            .compact_context(&c, &data, &a, "compact:1")
            .await
            .unwrap();
        assert_eq!(result.purpose, "resolve case");
        assert_eq!(result.unfinished, ["lookup"]);
        assert_eq!(result.facts[0].source, Source::Document);
        let op = s.list_operations(&c.run).await.unwrap().pop().unwrap();
        assert_eq!(op.request.kind, OperationKind::Compaction);
        assert!(op.settled);
        assert!(
            s.account(&s.get(&c.run).await.unwrap().spec.account)
                .await
                .unwrap()
                .used
                .tokens
                > 0
        );
    });
}
struct Rules;
impl TargetValidator for Rules {
    fn key(&self) -> ValidatorKey {
        ValidatorKey::new("host", "1")
    }
    fn check(&self, _: &Target) -> Result<Vec<Diagnostic>> {
        Ok(vec![])
    }
}
struct SlowRules {
    delay: Duration,
    version: &'static str,
    calls: Arc<AtomicUsize>,
}
impl TargetValidator for SlowRules {
    fn key(&self) -> ValidatorKey {
        ValidatorKey::new("slow-host", self.version)
    }
    fn check(&self, _: &Target) -> Result<Vec<Diagnostic>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        Ok(vec![])
    }
}
#[derive(Clone)]
struct WallClock(Instant);
impl Clock for WallClock {
    fn now(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }
    async fn wait_until(&self, deadline: u64) {
        tokio::time::sleep(Duration::from_millis(deadline.saturating_sub(self.now()))).await;
    }
}
fn validation_context(claim: &Claim, permission: &str) -> ToolContext {
    ToolContext {
        recovery_proof: None,
        claim: claim.clone(),
        principal: "judge".into(),
        operation: "validation-op".into(),
        attempt: 1,
        deadline: None,
        permission: permission.into(),
        account_binding: "local-validation".into(),
    }
}
async fn validation_fixture(
    delay: Duration,
    timeout: u64,
) -> (
    Arc<MemoryStore>,
    Claim,
    DataStore,
    Access,
    ValidationTool<SlowRules>,
) {
    let store = Arc::new(MemoryStore::default());
    let account = store
        .create_account("tenant", Amount::new(10000, 10000), None)
        .await
        .unwrap();
    let handle = store
        .start(RunSpec::new("tenant", "slow", account, "judge"))
        .await
        .unwrap();
    let claim = store.claim(&handle, "worker", 0, 100_000).await.unwrap();
    let data = DataStore::memory(StoragePolicy::default());
    let access = Access::owner(Namespace {
        owner: handle,
        agent: "main".into(),
    });
    let tool = ValidationTool {
        adapter: Arc::new(DataValidator {
            store: data.clone(),
            access: access.clone(),
            validator: SlowRules {
                delay,
                version: "1",
                calls: Arc::new(AtomicUsize::new(0)),
            },
            workspace: true,
            artifacts: vec![],
        }),
        run: store.get(&claim.run).await.unwrap(),
        timeout,
    };
    (store, claim, data, access, tool)
}
#[test]
fn slow_validation_over_thirty_seconds_applies_with_host_timeout() {
    block_on(async {
        let (store, claim, _, _, tool) =
            validation_fixture(Duration::from_millis(30_100), 35_000).await;
        let clock = WallClock(Instant::now());
        let rt = Runtime {
            store: store.clone(),
            model: FakeModel::scripted(vec![]),
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock,
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let descriptor = tool.descriptor();
        assert_eq!(descriptor.timeout, 35_000);
        assert_eq!(descriptor.retry.max_elapsed, 35_000);
        let input = json!({"candidate":"slow"});
        let mut request = DispatchRequest::new(
            "slow-validation",
            OperationKind::Tool,
            "validate",
            input.clone(),
            descriptor.reserve,
        );
        request.account_binding = descriptor.account;
        request.timeout = Some(descriptor.timeout);
        let invoke = &tool;
        let lookup = &tool;
        let output = rt
            .dispatch(
                &claim,
                request,
                "validate",
                |ctx| {
                    let input = input.clone();
                    async move { invoke.invoke(&ctx, input).await }
                },
                |ctx| {
                    let input = input.clone();
                    async move { lookup.lookup(&ctx, input).await }
                },
            )
            .await
            .unwrap();
        assert!(matches!(output, ExternalResult::Applied { .. }));
        assert!(store.list_operations(&claim.run).await.unwrap()[0].settled);
    });
}
#[test]
fn timed_out_validation_remains_unknown_even_after_evidence_arrives() {
    block_on(async {
        let (store, claim, data, access, tool) =
            validation_fixture(Duration::from_millis(150), 30).await;
        let clock = WallClock(Instant::now());
        let rt = Runtime {
            store: store.clone(),
            model: FakeModel::scripted(vec![]),
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock,
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let descriptor = tool.descriptor();
        let input = json!({"candidate":"late"});
        let mut request = DispatchRequest::new(
            "late-validation",
            OperationKind::Tool,
            "validate",
            input.clone(),
            descriptor.reserve,
        );
        request.account_binding = descriptor.account;
        request.timeout = Some(descriptor.timeout);
        let invoke = &tool;
        let lookup = &tool;
        let result = rt
            .dispatch(
                &claim,
                request.clone(),
                "validate",
                |ctx| {
                    let input = input.clone();
                    async move { invoke.invoke(&ctx, input).await }
                },
                |ctx| {
                    let input = input.clone();
                    async move { lookup.lookup(&ctx, input).await }
                },
            )
            .await
            .unwrap();
        assert_eq!(result, ExternalResult::Unknown);
        assert!(matches!(
            tool.lookup(&validation_context(&claim, "validate"), input.clone())
                .await
                .unwrap(),
            Lookup::Unknown
        ));
        assert!(!store.list_operations(&claim.run).await.unwrap()[0].settled);
        let target = data
            .run_target(&access, &tool.run, input.clone(), true, vec![])
            .unwrap();
        let wait_start = Instant::now();
        while data.evidence(&access, &target).unwrap().is_empty()
            && wait_start.elapsed() < Duration::from_secs(2)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(data.evidence(&access, &target).unwrap().len(), 1);
        assert!(matches!(
            tool.lookup(&validation_context(&claim, "reconcile"), input.clone())
                .await
                .unwrap(),
            Lookup::Unknown
        ));
        assert_eq!(tool.adapter.validator.calls.load(Ordering::SeqCst), 1);
        let mut wrong_account = validation_context(&claim, "validate");
        wrong_account.account_binding = "other".into();
        assert!(matches!(
            tool.lookup(&wrong_account, input.clone()).await,
            Err(Error::PermissionDenied)
        ));
        assert!(matches!(
            tool.lookup(
                &validation_context(&claim, "validate"),
                json!({"candidate":"different"})
            )
            .await
            .unwrap(),
            Lookup::Unknown
        ));
        data.write(&access, "new", None, b"changed").await.unwrap();
        assert!(matches!(
            tool.lookup(&validation_context(&claim, "validate"), input.clone())
                .await
                .unwrap(),
            Lookup::Unknown
        ));
        let recovered = rt
            .dispatch(
                &claim,
                request,
                "validate",
                |ctx| {
                    let input = input.clone();
                    async move { invoke.invoke(&ctx, input).await }
                },
                |ctx| {
                    let input = input.clone();
                    async move { lookup.lookup(&ctx, input).await }
                },
            )
            .await;
        assert!(matches!(recovered, Err(Error::UsageUnknown)));
    });
}
#[test]
fn same_validator_finalizer_contract_for_artifact_and_structured_judgment() {
    block_on(async {
        for workspace in [false, true] {
            let s = Arc::new(MemoryStore::default());
            let (h, c) = start(&s).await;
            let data = DataStore::memory(StoragePolicy::default());
            let a = Access::owner(Namespace {
                owner: h,
                agent: "main".into(),
            });
            if workspace {
                data.write(&a, "result", None, b"result").await.unwrap();
            }
            let artifacts = if workspace {
                vec![
                    data.put(&a, b"result", DataClass::Intermediate, 0)
                        .await
                        .unwrap(),
                ]
            } else {
                vec![]
            };
            let rt = Runtime {
                store: s,
                model: FakeModel::scripted(vec![FakeModel::decision(json!({"decision":"keep"}))]),
                tools: Vec::<BusinessTool>::new(),
                policy: FakePolicy::default(),
                clock: TestClock::default(),
                validator: std::sync::Arc::new(DataValidator {
                    store: data.clone(),
                    access: a.clone(),
                    validator: Rules,
                    workspace,
                    artifacts: artifacts.clone(),
                }),
                finalizer: std::sync::Arc::new(DataFinalizer {
                    store: data,
                    access: a,
                    required: vec![ValidatorKey::new("host", "1")],
                    workspace,
                    artifacts,
                }),
            };
            assert_eq!(
                rt.drive(&c).await.unwrap().state,
                RunState::Terminal(Terminal::Succeeded)
            );
        }
    });
}
struct DenyWrites;
impl Policy for DenyWrites {
    fn authorize(&self, _: &RunRecord, _: &str, permission: &str) -> Result<()> {
        if permission == "workspace.write" {
            Err(Error::PermissionDenied)
        } else {
            Ok(())
        }
    }
}
#[test]
fn catalog_filters_authority_and_rejects_changed_descriptor() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (h, c) = start(&s).await;
        let data = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(Namespace {
            owner: h,
            agent: "main".into(),
        });
        let tool = FileTool {
            store: data,
            access: a,
            action: FileAction::Write,
        };
        assert!(
            pin_catalog(
                &s.get(&c.run).await.unwrap(),
                &DenyWrites,
                std::slice::from_ref(&tool)
            )
            .unwrap()
            .is_empty()
        );
        let changed = Arc::new(AtomicBool::new(false));
        let dynamic = ChangingTool {
            changed: changed.clone(),
        };
        let model = ChangesCatalog {
            inner: FakeModel::scripted(vec![FakeModel::calls(vec![ToolCall {
                id: "a".into(),
                name: "change".into(),
                arguments: json!({}),
            }])]),
            changed,
        };
        let rt = Runtime {
            store: s.clone(),
            model,
            tools: vec![dynamic],
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: std::sync::Arc::new(DecisionValidator::default()),
            finalizer: std::sync::Arc::new(DecisionFinalizer),
        };
        assert!(rt.drive(&c).await.is_err());
        assert_eq!(s.list_operations(&c.run).await.unwrap().len(), 1);
    });
}
struct ChangingTool {
    changed: Arc<AtomicBool>,
}
impl Tool for ChangingTool {
    fn descriptor(&self) -> ToolDescriptor {
        let mut d = BusinessTool::new("change").descriptor();
        if self.changed.load(Ordering::SeqCst) {
            d.account = "different".into();
        }
        d
    }
    async fn invoke(&self, _: &ToolContext, _: Value) -> Result<ExternalResult> {
        panic!("changed implementation invoked")
    }
    async fn lookup(&self, _: &ToolContext, _: Value) -> Result<Lookup> {
        Ok(Lookup::Unknown)
    }
}
struct ChangesCatalog {
    inner: FakeModel,
    changed: Arc<AtomicBool>,
}
impl ModelPort for ChangesCatalog {
    fn limits(&self) -> ModelLimits {
        self.inner.limits()
    }
    fn input_upper_bound(&self, r: &PreparedModelRequest) -> Result<u64> {
        self.inner.input_upper_bound(r)
    }
    fn recovery(&self) -> Option<(String, u64)> {
        self.inner.recovery()
    }
    async fn invoke(&self, c: &ToolContext, r: PreparedModelRequest) -> Result<ExternalResult> {
        let out = self.inner.invoke(c, r).await;
        self.changed.store(true, Ordering::SeqCst);
        out
    }
    async fn lookup(&self, c: &ToolContext, r: PreparedModelRequest) -> Result<Lookup> {
        self.inner.lookup(c, r).await
    }
}
#[test]
fn exact_replace_schema_describes_and_validates_each_edit() {
    let store = DataStore::memory(StoragePolicy::default());
    let tool = FileTool {
        store,
        access: Access::owner(Namespace {
            owner: RunHandle {
                tenant: "t".into(),
                id: "r".into(),
            },
            agent: "main".into(),
        }),
        action: FileAction::ExactReplace,
    };
    let schema = tool.descriptor().input_schema;
    assert!(
        validate_schema(
            &json!({"expected_revision":1,"edits":[{"path":"a"}]}),
            &schema
        )
        .is_err()
    );
    assert!(schema["properties"]["edits"]["items"]["required"].is_array());
}
#[test]
fn validator_tool_and_output_wrapper_use_common_dispatch() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (h, c) = start(&s).await;
        let data = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(Namespace {
            owner: h,
            agent: "main".into(),
        });
        let rt = runtime(s.clone(), FakeModel::scripted(vec![]), vec![]);
        let tool = ValidationTool {
            adapter: Arc::new(DataValidator {
                store: data.clone(),
                access: a.clone(),
                validator: Rules,
                workspace: false,
                artifacts: vec![],
            }),
            run: s.get(&c.run).await.unwrap(),
            timeout: 30_000,
        };
        let d = tool.descriptor();
        let mut req = DispatchRequest::new(
            "validate",
            OperationKind::Tool,
            &d.name,
            json!({"decision":"keep"}),
            d.reserve,
        );
        req.account_binding = d.account;
        let tool_ref = &tool;
        let lookup_ref = &tool;
        let result = rt
            .dispatch(
                &c,
                req,
                "validate",
                |ctx| async move { tool_ref.invoke(&ctx, json!({"decision":"keep"})).await },
                |ctx| async move { lookup_ref.lookup(&ctx, json!({})).await },
            )
            .await
            .unwrap();
        let ExternalResult::Applied { output, .. } = result else {
            panic!("validation result")
        };
        assert_eq!(output["passed"], true);
        let wrapper = StoredOutputTool {
            inner: BusinessTool::new("case"),
            store: data.clone(),
            access: a.clone(),
            inline_limit: 1,
            now: 0,
        };
        let d = wrapper.descriptor();
        let mut req =
            DispatchRequest::new("output", OperationKind::Tool, &d.name, json!({}), d.reserve);
        req.account_binding = d.account;
        let invoke = &wrapper;
        let lookup = &wrapper;
        let result = rt
            .dispatch(
                &c,
                req,
                &d.permission,
                |ctx| async move { invoke.invoke(&ctx, json!({})).await },
                |ctx| async move { lookup.lookup(&ctx, json!({})).await },
            )
            .await
            .unwrap();
        let ExternalResult::Applied { output, .. } = result else {
            panic!("output result")
        };
        let stored: StoredOutput = serde_json::from_value(output).unwrap();
        assert!(matches!(stored, StoredOutput::External { .. }));
    });
}
#[test]
fn lost_file_response_keeps_saved_content_and_does_not_replay() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let (h, c) = start(&s).await;
        let data = DataStore::memory(StoragePolicy::default());
        let a = Access::owner(Namespace {
            owner: h,
            agent: "main".into(),
        });
        let tool = FileTool {
            store: data.clone(),
            access: a.clone(),
            action: FileAction::Write,
        };
        let d = tool.descriptor();
        let input = json!({"path":"a","expected_revision":null,"text":"saved"});
        let mut req = DispatchRequest::new(
            "write",
            OperationKind::Tool,
            &d.name,
            input.clone(),
            d.reserve,
        );
        req.version = d.version;
        req.account_binding = d.account;
        req.scope = Some(catalog_digest(&tool.descriptor()));
        let rt = Runtime {
            store: s.clone(),
            model: FakeModel::scripted(vec![]),
            tools: vec![tool.clone()],
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: std::sync::Arc::new(DecisionValidator::default()),
            finalizer: std::sync::Arc::new(DecisionFinalizer),
        };
        let invoke = &tool;
        let lookup = &tool;
        let lost = input.clone();
        rt.dispatch(
            &c,
            req,
            &d.permission,
            |ctx| async move {
                invoke.invoke(&ctx, lost).await?;
                Err(Error::Storage)
            },
            |ctx| async move { lookup.lookup(&ctx, input).await },
        )
        .await
        .unwrap();
        rt.reconcile(&c).await.unwrap();
        assert_eq!(data.snapshot(&a).await.unwrap().revision, 1);
        assert_eq!(
            s.list_operations(&c.run).await.unwrap()[0].state,
            OperationState::Unknown
        );
    });
}
