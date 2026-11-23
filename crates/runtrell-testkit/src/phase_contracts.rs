// SPDX-License-Identifier: Apache-2.0
use crate::*;
use runtrell::data::{Access, DataStore, StoragePolicy};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const FIRST_INPUT: &str = "source-only material";
const FIRST_TOOL: &str = "source_lookup";
const NEXT_TOOL: &str = "publish_preview";

struct Handoff {
    prior_workspace: DataStore,
    tries: AtomicUsize,
}

impl Finalizer for Handoff {
    fn finalize(
        &self,
        run: &RunRecord,
        output: Value,
        receipt: ValidationReceipt,
    ) -> Result<Completion> {
        DecisionFinalizer.finalize(run, output, receipt)
    }

    fn prepare_next_phase(&self, run: &RunRecord, seal: &PhaseSeal) -> Result<PhaseInput> {
        assert!(run.checkpoint.is_none());
        assert!(run.spec.input.is_empty());
        assert!(run.active_phase().is_none());
        assert_eq!(run.next_phase().unwrap().id, "render");
        assert_eq!(seal.phase, "collect");
        assert_eq!(seal.output, json!({"decision":"sealed"}));
        assert_eq!(seal.receipt.hash, digest(&seal.output));
        if self.tries.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(Error::Pending("phase payload purge".into()));
        }
        self.prior_workspace.erase_run(&run.handle)?;
        Ok(PhaseInput {
            input: "validated projection".into(),
            images: vec![],
        })
    }
}

pub async fn intermediate_completion_reopens_only_after_purge<S: ControlStore>(
    store: Arc<S>,
    reopened: Arc<S>,
) {
    let account = store
        .create_account("t", Amount::new(10_000, 10_000), None)
        .await
        .unwrap();
    let mut spec = RunSpec::new("t", "phase", account.clone(), FIRST_INPUT);
    spec.phases = vec![
        PhaseConfig {
            id: "collect".into(),
            tools: [FIRST_TOOL.into()].into(),
            completion: CompletionPolicy::default(),
        },
        PhaseConfig {
            id: "render".into(),
            tools: [NEXT_TOOL.into()].into(),
            completion: CompletionPolicy::default(),
        },
    ];
    let run = store.start(spec.clone()).await.unwrap();
    let first = store.claim(&run, "first", 0, 100).await.unwrap();
    let prior_workspace = DataStore::memory(StoragePolicy::default());
    let prior_access = Access::owner(Namespace {
        owner: run.clone(),
        agent: "collect".into(),
    });
    prior_workspace
        .write(&prior_access, "input.txt", None, FIRST_INPUT.as_bytes())
        .await
        .unwrap();
    let model = FakeModel::scripted(vec![
        FakeModel::calls(vec![ToolCall {
            id: "call".into(),
            name: FIRST_TOOL.into(),
            arguments: json!({"effect_key":"phase-only-tool-input"}),
        }]),
        FakeModel::decision(json!({"decision":"sealed"})),
        FakeModel::calls(vec![ToolCall {
            id: "call".into(),
            name: NEXT_TOOL.into(),
            arguments: json!({"effect_key":"next-phase-action"}),
        }]),
        FakeModel::decision(json!({"decision":"finished"})),
    ]);
    let clock = TestClock::default();
    let mut runtime = Runtime {
        store: store.clone(),
        model: model.clone(),
        tools: vec![BusinessTool::new(FIRST_TOOL), BusinessTool::new(NEXT_TOOL)],
        policy: FakePolicy::default(),
        clock: clock.clone(),
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(Handoff {
            prior_workspace: prior_workspace.clone(),
            tries: AtomicUsize::new(0),
        }),
    };
    let sealed = runtime.drive(&first).await.unwrap();
    assert_eq!(sealed.state, RunState::Finalizing);
    assert_eq!(sealed.handle, run);
    assert_eq!(sealed.spec.account, account);
    assert!(sealed.checkpoint.is_none());
    assert!(sealed.completion.is_none());
    assert_eq!(store.find_request(&spec).await.unwrap(), Some(run.clone()));
    assert_eq!(store.start(spec).await.unwrap(), run);
    assert_eq!(
        store.check(&first, 1, false).await,
        Err(Error::OwnershipLost)
    );

    let ledger = store.list_operations(&run).await.unwrap();
    assert_eq!(ledger.len(), 3);
    for op in ledger {
        assert!(op.request.input.is_null());
        assert_eq!(op.finished_at, Some(0));
        assert_eq!(store.operation(&run, &op.request.key).await.unwrap(), None);
    }
    assert_eq!(store.account(&account).await.unwrap().used.tokens, 8);

    runtime.store = reopened.clone();
    clock.set(1);
    let interrupted = reopened.claim(&run, "purge", 1, 2).await.unwrap();
    runtime.drive(&interrupted).await.unwrap();
    assert_eq!(model.requests.lock().unwrap().len(), 2);
    assert_eq!(
        prior_workspace
            .list(&prior_access, None, 10)
            .await
            .unwrap()
            .entries
            .len(),
        1
    );
    assert!(matches!(
        store.check(&interrupted, 1, true).await,
        Err(Error::Pending(_))
    ));

    clock.set(3);
    let resumed = reopened.claim(&run, "resume", 3, 100).await.unwrap();
    let done = runtime.drive(&resumed).await.unwrap();
    assert_eq!(done.state, RunState::Terminal(Terminal::Succeeded));
    assert_eq!(
        prior_workspace.list(&prior_access, None, 10).await,
        Err(Error::Closed)
    );
    assert_eq!(done.handle, run);
    assert_eq!(done.phase.as_ref().unwrap().seals[0].purged_at, Some(3));
    assert_eq!(store.account(&account).await.unwrap().used.tokens, 16);
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].tools[0].name, FIRST_TOOL);
    assert_eq!(requests[2].tools.len(), 1);
    assert_eq!(requests[2].tools[0].name, NEXT_TOOL);
    assert_eq!(
        requests[2].messages,
        vec![Message::text("user", "validated projection")]
    );
    assert!(requests[2].images.is_empty());
    assert!(requests[2].attachments.is_empty());
}

/// Exercise the same phase/child boundary with both control stores.
pub async fn children_are_bound_to_the_open_phase<S: ControlStore>(
    store: Arc<S>,
    reopened: Arc<S>,
) {
    let account = store
        .create_account("t", Amount::new(10_000, 10_000), None)
        .await
        .unwrap();
    let mut spec = RunSpec::new("t", "parent", account.clone(), FIRST_INPUT);
    spec.deadline = Some(300);
    spec.phases = ["collect", "publish"]
        .into_iter()
        .map(|id| PhaseConfig {
            id: id.into(),
            tools: Default::default(),
            completion: CompletionPolicy::default(),
        })
        .collect();
    let parent = store.start(spec.clone()).await.unwrap();
    let first = store.claim(&parent, "first", 0, 10).await.unwrap();
    let child_spec = RunSpec::new("t", "child", "overridden".into(), "child-only prompt");
    let rt = Runtime {
        store: reopened.clone(),
        model: FakeModel::default(),
        tools: Vec::<BusinessTool>::new(),
        policy: FakePolicy::default(),
        clock: TestClock::default(),
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    let (created, duplicate) = tokio::join!(
        store.admit_child(&first, "review", child_spec.clone(), 0),
        rt.spawn_child(&first, "review", child_spec.clone())
    );
    let child = created.unwrap();
    assert_eq!(duplicate.unwrap(), child);
    let initial = store.get(&child).await.unwrap();
    assert_eq!(initial.spec.parent, Some(parent.clone()));
    assert_eq!(initial.spec.account, account);
    assert_eq!(initial.spec.deadline, Some(300));
    assert_eq!(initial.spec.input, "child-only prompt");
    assert!(initial.checkpoint.is_none());
    assert!(initial.phase.is_none());
    let mut bypass = RunSpec::new("t", "bypass", account.clone(), "direct child");
    bypass.parent = Some(parent.clone());
    assert!(store.start(bypass).await.is_err());

    rt.clock.set(10);
    let current = reopened
        .claim(&parent, "replacement", 10, 100)
        .await
        .unwrap();
    assert_eq!(
        rt.join_child(&first, "review").await,
        Err(Error::OwnershipLost)
    );
    assert_eq!(
        rt.spawn_child(&current, "review", child_spec.clone())
            .await
            .unwrap(),
        child
    );
    let mut changed = child_spec.clone();
    changed.input = "different".into();
    assert_eq!(
        rt.spawn_child(&current, "review", changed).await,
        Err(Error::Conflict)
    );
    assert!(matches!(
        rt.join_child(&current, "review").await,
        Err(Error::Pending(_))
    ));

    let before = store.get(&parent).await.unwrap();
    let output = json!({"decision":"sealed"});
    let receipt = rt.validator.validate(&before, &output).unwrap();
    assert!(matches!(
        store
            .seal_phase(
                &current,
                before.revision,
                output.clone(),
                receipt.clone(),
                10
            )
            .await,
        Err(Error::Pending(_))
    ));
    assert_eq!(store.get(&parent).await.unwrap(), before);
    assert!(!store.list_operations(&parent).await.unwrap()[0].settled);

    let child_claim = store.claim(&child, "child-worker", 10, 100).await.unwrap();
    let attempt = store
        .begin(
            &child_claim,
            DispatchRequest::new(
                "work",
                OperationKind::Model,
                "work",
                Value::Null,
                Amount::new(20, 20),
            ),
            10,
        )
        .await
        .unwrap();
    store
        .mark_sent(&child_claim, &attempt.id, 10)
        .await
        .unwrap();
    store
        .record(
            &child_claim,
            &attempt.id,
            ExternalResult::Applied {
                output: json!({"result":"ready"}),
                usage: Usage::measured(3, 2, 7),
                physical_evidence: None,
                external_id: None,
            },
            10,
        )
        .await
        .unwrap();
    let child_record = store.get(&child).await.unwrap();
    let child_output = json!({"decision":"approved"});
    let child_receipt = rt.validator.validate(&child_record, &child_output).unwrap();
    let completion = DecisionFinalizer
        .finalize(&child_record, child_output, child_receipt)
        .unwrap();
    store.finalize(&child_claim, completion, 10).await.unwrap();
    let joined = rt.join_child(&current, "review").await.unwrap();
    assert_eq!(joined.state, RunState::Terminal(Terminal::Succeeded));
    assert_eq!(rt.join_child(&current, "review").await.unwrap(), joined);
    assert_eq!(
        store.account(&account).await.unwrap().used,
        Amount::new(5, 7)
    );
    let operation = store.list_operations(&parent).await.unwrap().pop().unwrap();
    assert!(operation.settled);
    assert_eq!(operation.send_count, 0);

    let revision = store.get(&parent).await.unwrap().revision;
    let sealed = store
        .seal_phase(&current, revision, output, receipt, 10)
        .await
        .unwrap();
    assert!(sealed.checkpoint.is_none());
    assert!(sealed.spec.input.is_empty());
    assert!(
        store
            .operation(&parent, &operation.request.key)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        rt.join_child(&current, "review").await,
        Err(Error::OwnershipLost)
    );
    let next = reopened.claim(&parent, "next", 10, 100).await.unwrap();
    assert!(matches!(
        rt.spawn_child(&next, "review", child_spec.clone()).await,
        Err(Error::Pending(_))
    ));
    assert!(matches!(
        rt.join_child(&next, "review").await,
        Err(Error::Pending(_))
    ));
    let revision = store.get(&parent).await.unwrap().revision;
    store
        .open_phase(&next, revision, PhaseInput::default(), 10)
        .await
        .unwrap();
    assert_eq!(rt.join_child(&next, "review").await, Err(Error::NotFound));
    let other = rt
        .spawn_child(&next, "review", child_spec.clone())
        .await
        .unwrap();
    assert_ne!(other, child);

    let final_record = store.get(&parent).await.unwrap();
    let final_output = json!({"decision":"complete"});
    let final_receipt = rt.validator.validate(&final_record, &final_output).unwrap();
    let final_completion = DecisionFinalizer
        .finalize(&final_record, final_output, final_receipt)
        .unwrap();
    assert!(matches!(
        store.finalize(&next, final_completion, 10).await,
        Err(Error::Pending(_))
    ));

    store.cancel(&parent, "cancelled").await.unwrap();
    for key in ["review", "new"] {
        assert_eq!(
            rt.spawn_child(&next, key, child_spec.clone()).await,
            Err(Error::Closed)
        );
    }
    assert_eq!(rt.join_child(&next, "review").await, Err(Error::Closed));
    assert_eq!(
        store.get(&other).await.unwrap().state,
        RunState::Terminal(Terminal::Cancelled("cancelled".into()))
    );
    rt.reconcile(&next).await.unwrap();
    assert!(
        store
            .list_operations(&parent)
            .await
            .unwrap()
            .iter()
            .all(|op| op.settled)
    );
    assert_eq!(
        store.account(&account).await.unwrap().used,
        Amount::new(5, 7)
    );

    spec.request_key = "failing-parent".into();
    let failing = store.start(spec).await.unwrap();
    let claim = store.claim(&failing, "worker", 10, 100).await.unwrap();
    let child = rt.spawn_child(&claim, "review", child_spec).await.unwrap();
    let failure = Terminal::Failed("parent stopped".into());
    store.terminate(&claim, failure.clone(), 10).await.unwrap();
    assert_eq!(
        store.get(&child).await.unwrap().state,
        RunState::Terminal(failure)
    );
    assert_eq!(rt.join_child(&claim, "review").await, Err(Error::Closed));
    rt.reconcile(&claim).await.unwrap();
    assert!(
        store
            .list_operations(&failing)
            .await
            .unwrap()
            .iter()
            .all(|op| op.settled)
    );
}
