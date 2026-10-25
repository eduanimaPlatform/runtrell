// SPDX-License-Identifier: Apache-2.0
use runtrell::{assembly::*, memory::MemoryStore, *};
use runtrell_testkit::*;
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};
fn chain() -> ValidationChain {
    ValidationChain {
        validators: vec![],
        schema: json!({"type":"object","required":["answer"],"properties":{"answer":{"type":"integer","minimum":1}}}),
        repair: RepairPolicy::default(),
        repairable_codes: BTreeSet::from(["SCHEMA_VALIDATION".into()]),
    }
}
async fn setup(budget: u64, deadline: Option<u64>) -> (Arc<MemoryStore>, Claim, String) {
    let store = Arc::new(MemoryStore::default());
    let account = store
        .create_account_with_policy(
            "tenant",
            Amount::new(budget, budget),
            None,
            AccountPolicy {
                finalize_after_stop: true,
                ..AccountPolicy::default()
            },
        )
        .await
        .unwrap();
    let mut spec = RunSpec::new("tenant", "repair", account.clone(), "answer");
    spec.deadline = deadline;
    let run = store.start(spec).await.unwrap();
    let claim = store.claim(&run, "worker", 0, 100).await.unwrap();
    (store, claim, account)
}
fn runtime_with(
    store: Arc<MemoryStore>,
    model: FakeModel,
    validator: ValidationChain,
) -> Runtime<
    MemoryStore,
    FakeModel,
    BusinessTool,
    FakePolicy,
    TestClock,
    ValidationChain,
    DecisionFinalizer,
> {
    Runtime {
        store,
        model,
        tools: vec![],
        policy: FakePolicy::default(),
        clock: TestClock::default(),
        validator: std::sync::Arc::new(validator),
        finalizer: std::sync::Arc::new(DecisionFinalizer),
    }
}
#[test]
fn candidate_and_diagnostics_survive_worker_loss_and_revalidation() {
    block_on(async {
        let (store, claim, account) = setup(1000, None).await;
        let model = FakeModel::scripted(vec![
            FakeModel::decision(json!({"answer":0})),
            FakeModel::decision(json!({"answer":2})),
        ]);
        let rt = runtime_with(store.clone(), model.clone(), chain());
        let waiting = rt.drive(&claim).await.unwrap();
        assert_eq!(waiting.state, RunState::Waiting("validation repair".into()));
        assert_eq!(store.slot_count(OperationKind::Run).await, 0);
        assert_eq!(store.slot_count(OperationKind::Model).await, 0);
        let cp = waiting.checkpoint.unwrap();
        assert_eq!(cp.payload["repair"]["candidate"], json!({"answer":0}));
        assert_eq!(
            cp.payload["repair"]["receipt"]["diagnostics"][0]["path"],
            "/answer"
        );
        drop(rt);
        let claim = store
            .claim(&claim.run, "new-worker", 101, 100)
            .await
            .unwrap();
        let rt = runtime_with(store.clone(), model.clone(), chain());
        rt.clock.set(101);
        let done = rt.drive(&claim).await.unwrap();
        assert_eq!(done.state, RunState::Terminal(Terminal::Succeeded));
        assert_eq!(done.output, Some(json!({"answer":2})));
        assert!(done.receipt.unwrap().passed);
        let operations = store.list_operations(&claim.run).await.unwrap();
        assert_eq!(operations.len(), 2);
        assert!(
            operations
                .iter()
                .all(|op| op.settled && op.request.reserve.tokens > 0)
        );
        assert_eq!(store.account(&account).await.unwrap().used.tokens, 8);
        assert_eq!(model.requests.lock().unwrap().len(), 2);
        assert!(
            model.requests.lock().unwrap()[1]
                .messages
                .iter()
                .any(|m| m.text.contains("SCHEMA_VALIDATION"))
        );
    });
}
#[test]
fn host_limits_unrepairable_budget_and_deadline_stop_without_extra_calls() {
    block_on(async {
        for mode in [
            "count",
            "time",
            "unrepairable",
            "budget",
            "deadline",
            "elapsed-after-wait",
        ] {
            let (store, claim, _) = setup(
                if mode == "budget" { 22 } else { 1000 },
                if mode == "deadline" { Some(10) } else { None },
            )
            .await;
            let model = FakeModel::scripted(vec![
                FakeModel::decision(json!({"answer":0})),
                FakeModel::decision(json!({"answer":3})),
            ]);
            let mut validator = chain();
            if mode == "count" {
                validator.repair.max_repairs = Some(0);
            }
            if mode == "time" {
                validator.repair.max_elapsed = Some(0);
            }
            if mode == "elapsed-after-wait" {
                validator.repair.max_elapsed = Some(5);
            }
            if mode == "unrepairable" {
                validator.repairable_codes.clear();
            }
            let rt = runtime_with(store.clone(), model.clone(), validator);
            let first = rt.drive(&claim).await.unwrap();
            if matches!(mode, "count" | "time" | "unrepairable") {
                assert!(matches!(
                    first.state,
                    RunState::Terminal(Terminal::Failed(_))
                ));
                assert_eq!(
                    first.checkpoint.unwrap().payload["last_validation"]["receipt"]["diagnostics"]
                        [0]["path"],
                    "/answer"
                );
            } else {
                assert!(matches!(first.state, RunState::Waiting(_)));
                if mode == "deadline" {
                    rt.clock.set(10);
                }
                if mode == "elapsed-after-wait" {
                    rt.clock.set(5);
                }
                if mode == "budget" {
                    store.exhaust(&claim, 0).await.unwrap();
                    store.finalization_allowed(&claim, 0).await.unwrap();
                }
                let result = rt.drive(&claim).await;
                if mode == "budget" {
                    assert!(matches!(
                        result.unwrap().state,
                        RunState::Terminal(Terminal::BudgetReached(_))
                    ));
                } else if mode == "deadline" {
                    assert_eq!(
                        result.unwrap().state,
                        RunState::Terminal(Terminal::DeadlineReached)
                    );
                } else {
                    assert!(matches!(
                        result.unwrap().state,
                        RunState::Terminal(Terminal::Failed(_))
                    ));
                }
            }
            assert_eq!(model.requests.lock().unwrap().len(), 1, "{mode}");
        }
    });
}
struct BrokenValidator(Error);
impl Validator for BrokenValidator {
    fn validate(&self, _: &RunRecord, _: &Value) -> Result<ValidationReceipt> {
        Err(self.0.clone())
    }
}
#[test]
fn infrastructure_and_authority_errors_never_become_repair_feedback() {
    block_on(async {
        for error in [Error::Storage, Error::PermissionDenied, Error::Conflict] {
            let (store, claim, _) = setup(1000, None).await;
            let model = FakeModel::scripted(vec![FakeModel::decision(json!({"answer":0}))]);
            let rt = Runtime {
                store: store.clone(),
                model: model.clone(),
                tools: Vec::<BusinessTool>::new(),
                policy: FakePolicy::default(),
                clock: TestClock::default(),
                validator: std::sync::Arc::new(BrokenValidator(error.clone())),
                finalizer: std::sync::Arc::new(DecisionFinalizer),
            };
            let result = rt.drive(&claim).await;
            if error == Error::PermissionDenied {
                assert!(matches!(
                    result.unwrap().state,
                    RunState::Terminal(Terminal::Failed(_))
                ));
            } else {
                assert_eq!(result.unwrap_err(), error);
            }
            assert!(
                store
                    .get(&claim.run)
                    .await
                    .unwrap()
                    .checkpoint
                    .unwrap()
                    .payload["repair"]
                    .is_null()
            );
            assert_eq!(model.requests.lock().unwrap().len(), 1);
        }
    });
}
#[test]
fn repair_can_use_tools_without_reusing_previous_operation_keys() {
    block_on(async {
        let (store, claim, _) = setup(1000, None).await;
        let calls = |effect: &str| {
            FakeModel::calls(vec![ToolCall {
                id: "same-id".into(),
                name: "search".into(),
                arguments: json!({"effect_key":effect}),
            }])
        };
        let model = FakeModel::scripted(vec![
            calls("first"),
            FakeModel::decision(json!({"answer":0})),
            calls("second"),
            FakeModel::decision(json!({"answer":2})),
        ]);
        let tool = BusinessTool::new("search");
        let mut rt = runtime_with(store.clone(), model, chain());
        rt.tools.push(tool.clone());
        assert!(matches!(
            rt.drive(&claim).await.unwrap().state,
            RunState::Waiting(_)
        ));
        assert_eq!(
            rt.drive(&claim).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        assert_eq!(tool.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        let operations = store.list_operations(&claim.run).await.unwrap();
        assert_eq!(operations.len(), 6);
        assert_eq!(
            operations
                .iter()
                .filter(|op| op.request.kind == OperationKind::Tool)
                .count(),
            2
        );
    });
}
#[test]
fn invalid_tool_input_returns_schema_pointers_without_invocation() {
    block_on(async {
        let (store, claim, _) = setup(1000, None).await;
        let model = FakeModel::scripted(vec![
            FakeModel::calls(vec![ToolCall {
                id: "bad-input".into(),
                name: "search".into(),
                arguments: json!({"unexpected":true}),
            }]),
            FakeModel::decision(json!({"answer":2})),
        ]);
        let tool = BusinessTool::new("search");
        let rt = Runtime {
            store: store.clone(),
            model: model.clone(),
            tools: vec![StrictTool(tool.clone())],
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: std::sync::Arc::new(chain()),
            finalizer: std::sync::Arc::new(DecisionFinalizer),
        };
        assert_eq!(
            rt.drive(&claim).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        assert_eq!(tool.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        let requests = model.requests.lock().unwrap();
        let feedback = requests[1]
            .messages
            .iter()
            .find(|m| m.role == "tool")
            .unwrap();
        assert_eq!(feedback.data["code"], "TOOL_INPUT_INVALID");
        assert!(
            feedback.data["diagnostics"][0]["schema_path"]
                .as_str()
                .is_some()
        );
    });
}

struct StrictTool(BusinessTool);
impl Tool for StrictTool {
    fn descriptor(&self) -> ToolDescriptor {
        let mut descriptor = self.0.descriptor();
        descriptor.input_schema = json!({"type":"object","properties":{"query":{"type":"string"}},"required":["query"],"additionalProperties":false});
        descriptor
    }
    async fn invoke(&self, c: &ToolContext, v: Value) -> Result<ExternalResult> {
        self.0.invoke(c, v).await
    }
    async fn lookup(&self, c: &ToolContext, v: Value) -> Result<Lookup> {
        self.0.lookup(c, v).await
    }
}
