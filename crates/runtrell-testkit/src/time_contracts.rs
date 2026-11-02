// SPDX-License-Identifier: Apache-2.0
//! Time-unit contracts. Every `u64` time and duration in the control API is
//! Unix milliseconds on the host `Clock`; these contracts drive a fake clock, so
//! no real time passes, and cover just-before / equal / just-after boundaries,
//! sub-second values and overflow. Run them against every `ControlStore`.
use crate::*;

/// One Model permit, so a second Model reservation meets backpressure.
pub fn one_model_slot() -> Capacity {
    let mut capacity = Capacity::default();
    capacity.global.insert(OperationKind::Model, 1);
    capacity
}

async fn account<S: ControlStore>(store: &S) -> String {
    store
        .create_account("t", Amount::new(1_000_000, 1_000_000), None)
        .await
        .unwrap()
}

/// Lease expiry, renewal and Run deadline boundaries with sub-second leases.
pub async fn lease_and_deadline_boundaries<S: ControlStore>(store: Arc<S>) {
    let account = account(&*store).await;
    let mut spec = RunSpec::new("t", "lease-deadline", account, "judge");
    spec.deadline = Some(2_500);
    let run = store.start(spec).await.unwrap();

    // A 500 ms lease is half a second, not 500 seconds: lease_until = now + lease.
    let first = store.claim(&run, "first", 1_000, 500).await.unwrap();
    assert_eq!(first.lease_until, 1_500);
    assert!(store.check(&first, 1_499, false).await.is_ok());
    for now in [1_500, 1_501] {
        assert_eq!(
            store.check(&first, now, false).await,
            Err(Error::OwnershipLost),
            "now={now}"
        );
    }
    // A worker cannot take over until the lease is expired; equal is expired.
    assert_eq!(
        store.claim(&run, "second", 1_499, 500).await,
        Err(Error::OwnershipLost)
    );

    // Renewal moves the expiry forward only: max(old, now + lease).
    let renewed = store.renew(&first, 1_400, 500).await.unwrap();
    assert_eq!(renewed.lease_until, 1_900);
    let not_shorter = store.renew(&renewed, 1_410, 100).await.unwrap();
    assert_eq!(not_shorter.lease_until, 1_900);
    assert!(store.check(&not_shorter, 1_899, false).await.is_ok());
    assert_eq!(
        store.check(&not_shorter, 1_900, false).await,
        Err(Error::OwnershipLost)
    );
    assert_eq!(
        store.renew(&not_shorter, 1_900, 500).await,
        Err(Error::OwnershipLost),
        "an expired lease cannot be renewed"
    );

    // Takeover exactly at the expiry; the previous generation stays fenced out.
    assert_eq!(
        store.claim(&run, "second", 1_899, 500).await,
        Err(Error::OwnershipLost)
    );
    let second = store.claim(&run, "second", 1_900, 600).await.unwrap();
    assert_eq!(second.lease_until, 2_500);
    assert_eq!(second.generation, first.generation + 1);
    assert_eq!(
        store.check(&not_shorter, 1_900, false).await,
        Err(Error::OwnershipLost)
    );

    // Run deadline 2_500: new work is refused at and after it, never before.
    let live = store.renew(&second, 2_000, 10_000).await.unwrap();
    assert_eq!(live.lease_until, 12_000);
    assert!(store.check(&live, 2_499, true).await.is_ok());
    for now in [2_500, 2_501] {
        assert_eq!(
            store.check(&live, now, true).await,
            Err(Error::Deadline),
            "now={now}"
        );
    }
    // Existing work may still be checked after the deadline while the lease holds.
    assert!(store.check(&live, 2_500, false).await.is_ok());
}

/// Overflow in `now + lease` is refused as Conflict and changes nothing.
pub async fn lease_overflow_is_refused<S: ControlStore>(store: Arc<S>) {
    let account = account(&*store).await;
    let run = store
        .start(RunSpec::new("t", "lease-overflow", account, "judge"))
        .await
        .unwrap();
    assert_eq!(
        store.claim(&run, "worker", u64::MAX - 10, 100).await,
        Err(Error::Conflict)
    );
    assert_eq!(store.get(&run).await.unwrap().owner, None);
    assert_eq!(
        store.claim(&run, "worker", u64::MAX, 1).await,
        Err(Error::Conflict)
    );
    assert_eq!(
        store.claim(&run, "worker", 1_000, 0).await,
        Err(Error::Invalid("zero lease".into()))
    );
    // The largest representable expiry is accepted.
    let claim = store
        .claim(&run, "worker", u64::MAX - 100, 100)
        .await
        .unwrap();
    assert_eq!(claim.lease_until, u64::MAX);
    assert_eq!(
        store.renew(&claim, u64::MAX - 100, u64::MAX).await,
        Err(Error::Conflict)
    );
}

struct Recorder {
    inner: FakeModel,
    timeout: u64,
    deadlines: Arc<Mutex<Vec<Option<u64>>>>,
}
impl ModelPort for Recorder {
    fn limits(&self) -> ModelLimits {
        ModelLimits {
            timeout: self.timeout,
            ..self.inner.limits()
        }
    }
    fn input_upper_bound(&self, request: &PreparedModelRequest) -> Result<u64> {
        self.inner.input_upper_bound(request)
    }
    fn recovery(&self) -> Option<(String, u64)> {
        self.inner.recovery()
    }
    async fn invoke(
        &self,
        context: &ToolContext,
        request: PreparedModelRequest,
    ) -> Result<ExternalResult> {
        self.deadlines.lock().unwrap().push(context.deadline);
        self.inner.invoke(context, request).await
    }
    async fn lookup(&self, context: &ToolContext, request: PreparedModelRequest) -> Result<Lookup> {
        self.inner.lookup(context, request).await
    }
}

/// The operation deadline is `Clock::now() + timeout` (the earlier of it and the
/// Run deadline), and the recovery boundary is `now + retention`, in milliseconds.
pub async fn operation_deadline_is_now_plus_timeout_ms<S: ControlStore>(store: Arc<S>) {
    let now = 1_700_000_000_000;
    for (name, run_deadline, expected) in [
        ("none", None, now + 500),
        ("earlier-run-deadline", Some(now + 300), now + 300),
        ("later-run-deadline", Some(now + 900), now + 500),
    ] {
        let account = account(&*store).await;
        let mut spec = RunSpec::new("t", name, account, "judge");
        spec.deadline = run_deadline;
        let run = store.start(spec).await.unwrap();
        let claim = store.claim(&run, "worker", now, 60_000).await.unwrap();
        let seen = Arc::new(Mutex::new(vec![]));
        let clock = TestClock::default();
        clock.set(now);
        let rt = Runtime {
            store: store.clone(),
            model: Recorder {
                inner: FakeModel::scripted(vec![FakeModel::decision(json!({"decision":"keep"}))]),
                timeout: 500,
                deadlines: seen.clone(),
            },
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock,
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        assert_eq!(
            rt.drive(&claim).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded),
            "{name}"
        );
        assert_eq!(*seen.lock().unwrap(), vec![Some(expected)], "{name}");
        let operations = store.list_operations(&run).await.unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].request.timeout, Some(500));
        // FakeModel retains recovery for 1_000 ms.
        assert_eq!(operations[0].request.recovery_until, Some(now + 1_000));
        assert_eq!(operations[0].created_at, now);
    }
}

/// A Run without a deadline waits `CAPACITY_WAIT_MS` (60_000 ms) in the capacity
/// queue; the queue expires entries at, not before, their absolute deadline.
/// `store` must be built with [`one_model_slot`].
pub async fn capacity_wait_default_is_sixty_seconds_in_ms<S: ControlStore>(store: Arc<S>) {
    assert_eq!(CAPACITY_WAIT_MS, 60_000);
    let started_at = 5_000;
    // Another Run's Model call holds the only permit.
    store
        .acquire(SlotRequest {
            key: "hog".into(),
            tenant: "other".into(),
            run: "other".into(),
            kind: OperationKind::Model,
        })
        .await
        .unwrap();
    for (name, run_deadline, expected) in [
        ("default", None, started_at + CAPACITY_WAIT_MS),
        ("explicit", Some(started_at + 250), started_at + 250),
    ] {
        let account = account(&*store).await;
        let mut spec = RunSpec::new("t", name, account, "judge");
        spec.deadline = run_deadline;
        let run = store.start(spec).await.unwrap();
        let claim = store
            .claim(&run, "worker", started_at, 1_000_000)
            .await
            .unwrap();
        let clock = TestClock::default();
        clock.set(started_at);
        let rt = Runtime {
            store: store.clone(),
            model: FakeModel::scripted(vec![FakeModel::decision(json!({"decision":"keep"}))]),
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock,
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        assert_eq!(
            rt.drive(&claim).await.unwrap().state,
            RunState::Waiting("capacity".into()),
            "{name}"
        );
        // The queued absolute deadline is exactly start + 60_000 ms (or the Run's own).
        let entry = store.dequeue(started_at).await.unwrap().unwrap();
        assert_eq!(entry.deadline, expected, "{name}");
        // Just before the deadline it is dispatchable; at it, it has timed out.
        store.enqueue(entry.clone()).await.unwrap();
        let before = store.dequeue(expected - 1).await.unwrap().unwrap();
        assert_eq!(before.key, entry.key);
        store.enqueue(entry.clone()).await.unwrap();
        assert!(matches!(
            store.dequeue(expected).await,
            Err(Error::QueueTimeout)
        ));
        store.enqueue(entry.clone()).await.unwrap();
        assert!(matches!(
            store.dequeue(expected + 1).await,
            Err(Error::QueueTimeout)
        ));
        // Timed-out entries are removed; a fresh Run's claim above never mixed them.
        assert_eq!(
            store.dequeue(started_at).await.unwrap().map(|e| e.key),
            None
        );
    }
}
