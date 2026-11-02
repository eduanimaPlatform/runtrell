// SPDX-License-Identifier: Apache-2.0
//! Contracts for an Applied operation whose usage cannot settle: the Run reaches an
//! explicit operator-action wait instead of re-asking forever, and the owner can settle
//! the original operation once, idempotently and without a provider send.
use crate::image_contracts::{FakeImages, resolver};
use crate::*;
use runtrell::{data::*, images::*};

pub const MODEL: &str = "gpt-image-2.5-sunburst";
/// GPT Image 2.5 Standard rates, integer micro-USD per million tokens.
pub fn rates() -> Price {
    Price {
        model: MODEL.into(),
        input: 5_000_000,
        cached: 1_250_000,
        output: 30_000_000,
        reasoning: 30_000_000,
        image_input: 8_000_000,
        image_output: 30_000_000,
        cached_image_input: 2_000_000,
    }
}
/// The usage stored for a real image operation whose cached counter was missing.
pub fn stored_usage() -> Usage {
    Usage {
        model: MODEL.into(),
        input: Some(343),
        output: Some(601),
        image_input: Some(0),
        image_output: Some(601),
        quantities: [
            ("image_input_count".to_string(), 0),
            ("text_input_tokens".to_string(), 343),
            ("image_output_count".to_string(), 1),
            ("text_output_tokens".to_string(), 0),
        ]
        .into(),
        ..Usage::default()
    }
}
/// What the documented endpoint rule adds to `stored_usage`.
pub fn refined(mut usage: Usage) -> Usage {
    usage.cached = Some(0);
    usage.cached_image_input = Some(0);
    usage
}
pub fn recovery() -> SettlementRecovery {
    SettlementRecovery {
        basis: "test-images-rule-1".into(),
        usage: refined(stored_usage()),
        price: rates(),
    }
}
/// ceil((343 * 5 + 601 * 30) micro-USD).
pub const EXPECTED_MICRO_USD: u64 = 19_745;

type Rt<S> = Runtime<
    S,
    ImageModel<FakeModel, DataStore, FakePolicy, S>,
    ImageTool<FakeImages, DataStore, FakePolicy, S, TestClock>,
    FakePolicy,
    TestClock,
    DecisionValidator,
    DecisionFinalizer,
>;
struct Fixture {
    handle: RunHandle,
    account: String,
    provider: FakeImages,
    clock: TestClock,
    model: FakeModel,
    data: DataStore,
    access: Access,
}
impl Fixture {
    fn runtime<S: ControlStore + 'static>(&self, store: Arc<S>) -> Rt<S> {
        Runtime {
            store: store.clone(),
            model: ImageModel {
                inner: self.model.clone(),
                resolver: resolver(
                    self.data.clone(),
                    self.access.clone(),
                    ImageLimits::default(),
                ),
                runs: store.clone(),
            },
            tools: vec![ImageTool {
                provider: self.provider.clone(),
                resolver: resolver(
                    self.data.clone(),
                    self.access.clone(),
                    ImageLimits::default(),
                ),
                runs: store,
                clock: self.clock.clone(),
                output_access: self.access.clone(),
                config: ImageToolConfig {
                    action: ImageAction::Generate,
                    reserve: Amount::new(1_000, 50_000),
                    timeout: 30_000,
                },
            }],
            policy: FakePolicy::default(),
            clock: self.clock.clone(),
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        }
    }
    async fn op<S: ControlStore>(&self, store: &S) -> Operation {
        store
            .list_operations(&self.handle)
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.request.kind == OperationKind::Tool)
            .unwrap()
    }
}
async fn fixture<S: ControlStore + 'static>(store: Arc<S>, key: &str) -> Fixture {
    let account = store
        .create_account("images", Amount::new(1_000_000, 10_000_000), None)
        .await
        .unwrap();
    let handle = store
        .start(RunSpec::new("images", key, account.clone(), "draw"))
        .await
        .unwrap();
    let data = DataStore::memory(StoragePolicy::default());
    let access = Access::owner(Namespace {
        owner: handle.clone(),
        agent: "main".into(),
    });
    Fixture {
        handle,
        account,
        provider: FakeImages {
            usage: stored_usage(),
            ..Default::default()
        },
        clock: TestClock::default(),
        model: FakeModel::scripted(vec![
            FakeModel::calls(vec![ToolCall {
                id: "gen".into(),
                name: "image.generate".into(),
                arguments: json!({"prompt":"a red pixel"}),
            }]),
            FakeModel::decision(json!({"decision":"keep"})),
        ]),
        data,
        access,
    }
}

const DAY: u64 = 86_400_000;

/// The Run stops at the operator-action wait with the reservation held, nothing
/// re-asks the provider, the owner recovery settles once, and the Run then resumes.
pub async fn image_usage_operator_recovery<S: ControlStore + 'static>(
    store: Arc<S>,
    restarted: Arc<S>,
) {
    let f = fixture(store.clone(), "operator-recovery").await;
    f.clock.set(1_000);
    let claim = store
        .claim(&f.handle, "worker", 1_000, 60_000)
        .await
        .unwrap();
    let first = f.runtime(store.clone()).drive(&claim).await.unwrap();
    assert_eq!(first.state, RunState::Waiting(OPERATOR_ACTION_WAIT.into()));
    assert_eq!(f.provider.sends.load(Ordering::SeqCst), 1);
    let waiting = f.op(&*store).await;
    assert!(waiting.awaits_operator() && !waiting.settled && waiting.unknown_usage);
    assert!(waiting.recovery.is_none());
    let held = store.account(&f.account).await.unwrap();
    // The model call before the image tool is already settled; only the image is held.
    let baseline = held.used;
    assert_eq!(held.unknown, 1);
    assert_eq!(held.reserved, Amount::new(1_000, 50_000));

    // Hours and days later, from a restarted worker: same state, no send, no write.
    let mut now = 1_000;
    for (index, step) in [60_001, 3_600_000, DAY, 30 * DAY].into_iter().enumerate() {
        now += step;
        f.clock.set(now);
        let next = restarted
            .claim(&f.handle, &format!("restart-{index}"), now, 60_000)
            .await
            .unwrap();
        let record = f.runtime(restarted.clone()).drive(&next).await.unwrap();
        assert_eq!(record.state, RunState::Waiting(OPERATOR_ACTION_WAIT.into()));
        assert_eq!(f.provider.sends.load(Ordering::SeqCst), 1);
        let again = f.op(&*restarted).await;
        assert_eq!(again.finished_at, waiting.finished_at);
        assert_eq!(again.result, waiting.result);
        assert_eq!(again.send_count, 1);
        let held = restarted.account(&f.account).await.unwrap();
        assert_eq!((held.unknown, held.used), (1, baseline));
        assert_eq!(held.reserved, Amount::new(1_000, 50_000));
    }

    // Evidence that cannot be settled stays for the operator and changes nothing.
    let mut changes_known_counter = recovery();
    changes_known_counter.usage.input = Some(344);
    let mut other_model = recovery();
    other_model.price.model = "other".into();
    let mut drops_counter = recovery();
    drops_counter.usage.output = None;
    let mut keeps_priced = recovery();
    keeps_priced.usage.micro_usd = Some(0);
    let mut bad_basis = recovery();
    bad_basis.basis = "has space".into();
    for bad in [
        changes_known_counter,
        other_model,
        drops_counter,
        keeps_priced,
        bad_basis,
    ] {
        assert!(matches!(
            restarted
                .settle_recovered(&f.handle, &waiting.id, bad, now)
                .await,
            Err(Error::Invalid(_))
        ));
    }
    // Cached still unknown: the price cannot be applied, so the operation stays unsettled.
    let still_unknown = SettlementRecovery {
        usage: stored_usage(),
        ..recovery()
    };
    assert_eq!(
        restarted
            .settle_recovered(&f.handle, &waiting.id, still_unknown, now)
            .await,
        Err(Error::UsageUnknown)
    );
    // Another Run's handle cannot reach this operation.
    let foreign = RunHandle {
        tenant: "images".into(),
        id: "someone-else".into(),
    };
    assert_eq!(
        restarted
            .settle_recovered(&foreign, &waiting.id, recovery(), now)
            .await,
        Err(Error::NotFound)
    );
    assert_eq!(
        restarted
            .settle_recovered(&f.handle, "no-such-operation", recovery(), now)
            .await,
        Err(Error::NotFound)
    );
    let untouched = f.op(&*restarted).await;
    assert!(untouched.awaits_operator() && untouched.recovery.is_none());
    assert_eq!(untouched.result, waiting.result);
    let held = restarted.account(&f.account).await.unwrap();
    assert_eq!((held.unknown, held.used), (1, baseline));

    // Eight concurrent identical requests, spread over both handles: exactly one charge.
    let settled_at = now + 1;
    let tasks: Vec<_> = (0..8)
        .map(|i| {
            let target = if i % 2 == 0 {
                store.clone()
            } else {
                restarted.clone()
            };
            let (handle, id) = (f.handle.clone(), waiting.id.clone());
            tokio::spawn(async move {
                target
                    .settle_recovered(&handle, &id, recovery(), settled_at)
                    .await
            })
        })
        .collect();
    let mut records = vec![];
    for task in tasks {
        records.push(task.await.unwrap().unwrap());
    }
    assert!(records.windows(2).all(|pair| pair[0] == pair[1]));
    let record = records.remove(0);
    assert_eq!(record.basis, "test-images-rule-1");
    assert_eq!(record.price, rates());
    assert_eq!(record.previous, stored_usage());
    assert_eq!(record.settled.micro_usd, Some(EXPECTED_MICRO_USD));
    assert_eq!(record.settled.cached, Some(0));
    assert_eq!(record.amount, Amount::new(944, EXPECTED_MICRO_USD));
    assert_eq!(record.at, settled_at);

    let done = f.op(&*restarted).await;
    assert!(done.settled && !done.unknown_usage && !done.awaits_operator());
    assert!(done.unsettled_result.is_none() && done.usage_observations.is_empty());
    assert_eq!(done.recovery.as_deref(), Some(&record));
    assert_eq!(done.send_count, 1);
    match (&done.result, &waiting.result) {
        (
            Some(ExternalResult::Applied { usage, output, .. }),
            Some(ExternalResult::Applied {
                output: old_output, ..
            }),
        ) => {
            assert_eq!(usage, &record.settled);
            assert_eq!(output, old_output);
        }
        other => panic!("{other:?}"),
    }
    let account = restarted.account(&f.account).await.unwrap();
    assert_eq!(account.unknown, 0);
    assert_eq!(account.reserved, Amount::default());
    assert_eq!(account.used, baseline.checked_add(record.amount).unwrap());

    // Repeating the request, or a different basis, never charges or rewrites again.
    assert_eq!(
        store
            .settle_recovered(&f.handle, &waiting.id, recovery(), settled_at + 5)
            .await,
        Ok(record.clone())
    );
    let mut different = recovery();
    different.basis = "another-basis".into();
    assert_eq!(
        store
            .settle_recovered(&f.handle, &waiting.id, different, settled_at + 5)
            .await,
        Err(Error::Conflict)
    );
    assert_eq!(
        restarted.account(&f.account).await.unwrap().used,
        baseline.checked_add(record.amount).unwrap()
    );

    // The Run continues from the saved result: no image is generated again.
    now += 60_001; // the last restarted worker's lease has expired
    f.clock.set(now);
    let resumed = restarted
        .claim(&f.handle, "resume", now, 60_000)
        .await
        .unwrap();
    let finished = f.runtime(restarted.clone()).drive(&resumed).await.unwrap();
    assert_eq!(finished.state, RunState::Terminal(Terminal::Succeeded));
    assert_eq!(f.provider.sends.load(Ordering::SeqCst), 1);
    let tool_ops: Vec<_> = restarted
        .list_operations(&f.handle)
        .await
        .unwrap()
        .into_iter()
        .filter(|o| o.request.kind == OperationKind::Tool)
        .collect();
    assert_eq!(tool_ops.len(), 1);
    let account = restarted.account(&f.account).await.unwrap();
    assert_eq!(account.unknown, 0);
    assert_eq!(account.reserved, Amount::default());
}

/// A model operation whose Applied result carries no cost.
struct Unpriced {
    inner: FakeModel,
}
fn strip_cost(result: ExternalResult) -> ExternalResult {
    match result {
        ExternalResult::Applied {
            output,
            mut usage,
            external_id,
            physical_evidence,
        } => {
            usage.micro_usd = None;
            ExternalResult::Applied {
                output,
                usage,
                external_id,
                physical_evidence,
            }
        }
        other => other,
    }
}
impl ModelPort for Unpriced {
    fn limits(&self) -> ModelLimits {
        self.inner.limits()
    }
    fn input_upper_bound(&self, request: &PreparedModelRequest) -> Result<u64> {
        self.inner.input_upper_bound(request)
    }
    fn recovery(&self) -> Option<(String, u64)> {
        self.inner.recovery()
    }
    async fn invoke(&self, c: &ToolContext, r: PreparedModelRequest) -> Result<ExternalResult> {
        Ok(strip_cost(self.inner.invoke(c, r).await?))
    }
    async fn lookup(&self, c: &ToolContext, r: PreparedModelRequest) -> Result<Lookup> {
        Ok(match self.inner.lookup(c, r).await? {
            Lookup::Found(result) => Lookup::Found(strip_cost(result)),
            other => other,
        })
    }
}

/// While the adapter's lookup window is open (`recovery_until`, Unix ms) the Run keeps
/// asking; at the window's end, exactly, it moves to the operator-action wait.
pub async fn wait_follows_the_lookup_window<S: ControlStore + 'static>(store: Arc<S>) {
    let account = store
        .create_account("t", Amount::new(1_000_000, 1_000_000), None)
        .await
        .unwrap();
    let run = store
        .start(RunSpec::new("t", "lookup-window", account.clone(), "judge"))
        .await
        .unwrap();
    let start = 1_700_000_000_000;
    let clock = TestClock::default();
    clock.set(start);
    let claim = store.claim(&run, "worker", start, 10 * DAY).await.unwrap();
    let rt = Runtime {
        store: store.clone(),
        model: Unpriced {
            inner: FakeModel::scripted(vec![FakeModel::decision(json!({"decision":"keep"}))]),
        },
        tools: Vec::<BusinessTool>::new(),
        policy: FakePolicy::default(),
        clock: clock.clone(),
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    let state = rt.drive(&claim).await.unwrap().state;
    assert_eq!(state, RunState::Waiting("external reconciliation".into()));
    let op = store.list_operations(&run).await.unwrap().remove(0);
    // FakeModel pins a 1_000 ms recovery window.
    assert_eq!(op.request.recovery_until, Some(start + 1_000));
    assert!(op.awaits_operator() && !op.stalled(start));
    for (now, stalled) in [
        (start + 500, false),
        (start + 999, false),
        (start + 1_000, true),
        (start + 1_001, true),
        (start + DAY, true),
    ] {
        clock.set(now);
        let state = rt.drive(&claim).await.unwrap().state;
        let expected = if stalled {
            RunState::Waiting(OPERATOR_ACTION_WAIT.into())
        } else {
            RunState::Waiting("external reconciliation".into())
        };
        assert_eq!(state, expected, "now={now}");
        let op = store.list_operations(&run).await.unwrap().remove(0);
        assert_eq!(op.stalled(now), stalled, "now={now}");
        // Same evidence throughout: nothing was rewritten and nothing is released.
        assert!(!op.settled && op.unknown_usage && op.send_count == 1);
        let held = store.account(&account).await.unwrap();
        assert_eq!(held.unknown, 1);
        assert!(held.reserved.tokens > 0);
    }
    // Even after the window the operator is not forced: the owner recovery settles it.
    let op = store.list_operations(&run).await.unwrap().remove(0);
    let ExternalResult::Applied { usage, .. } = op.result.clone().unwrap() else {
        panic!()
    };
    let record = store
        .settle_recovered(
            &run,
            &op.id,
            SettlementRecovery {
                basis: "test-measured-elsewhere".into(),
                usage,
                price: FakeModel::default().limits().price.unwrap(),
            },
            start + DAY,
        )
        .await
        .unwrap();
    assert_eq!(record.amount, Amount::new(4, 4));
    let account = store.account(&account).await.unwrap();
    assert_eq!((account.unknown, account.used), (0, Amount::new(4, 4)));
}
