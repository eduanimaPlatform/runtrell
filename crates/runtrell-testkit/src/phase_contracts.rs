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
