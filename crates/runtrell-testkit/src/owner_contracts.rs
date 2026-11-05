// SPDX-License-Identifier: Apache-2.0
//! Owner commands must be fenced and have durable, immutable causal receipts.
use crate::*;

fn bx<'a, T, F: std::future::Future<Output = T> + Send + 'a>(
    make: impl FnOnce() -> F,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>> {
    Box::pin(make())
}
pub fn command(key: &str) -> OwnerCommand {
    OwnerCommand {
        key: key.into(),
        request_digest: digest(&json!(["owner-test", key])),
    }
}
pub async fn held<S: ControlStore>(store: &S, key: &str, deadline: Option<u64>) -> RunHandle {
    let account = bx(|| store.create_account("owner", Amount::new(100, 100), None))
        .await
        .unwrap();
    let mut spec = RunSpec::new("owner", key, account, "test");
    spec.deadline = deadline;
    let run = bx(|| store.start(spec)).await.unwrap();
    let claim = bx(|| store.claim(&run, "worker", 0, 10)).await.unwrap();
    bx(|| store.wait(&claim, OPERATOR_ACTION_WAIT, 0))
        .await
        .unwrap();
    run
}

pub async fn cancelled_resume_is_refused<S: ControlStore>(store: Arc<S>) {
    let run = held(&*store, "cancelled", None).await;
    bx(|| store.cancel(&run, "owner cancelled")).await.unwrap();
    let before = bx(|| store.get(&run)).await.unwrap();
    assert_eq!(
        bx(|| store.resume_held(&run, before.revision, command("cancelled"), 10)).await,
        Err(Error::Closed)
    );
    assert_eq!(bx(|| store.get(&run)).await.unwrap(), before);
    assert_eq!(bx(|| store.owner_receipt("cancelled")).await.unwrap(), None);
}

/// Fresh commands check every precondition without leaving a receipt or mutation.
pub async fn resume_preconditions<S: ControlStore>(store: Arc<S>) {
    for (key, expected_error, now, deadline, stale, queued, terminal) in [
        ("stale", Error::Conflict, 10, None, true, false, false),
        ("live", Error::OwnershipLost, 9, None, false, false, false),
        (
            "deadline",
            Error::Deadline,
            10,
            Some(10),
            false,
            false,
            false,
        ),
        ("not-held", Error::NotHeld, 10, None, false, true, false),
        ("terminal", Error::Closed, 10, None, false, false, true),
    ] {
        let run = held(&*store, key, deadline).await;
        if queued {
            let rev = bx(|| store.get(&run)).await.unwrap().revision;
            bx(|| store.resume_held(&run, rev, command("first-resume"), 10))
                .await
                .unwrap();
        }
        if terminal {
            let claim = bx(|| store.claim(&run, "terminal", 10, 10)).await.unwrap();
            bx(|| store.terminate(&claim, Terminal::DeadlineReached, 10))
                .await
                .unwrap();
        }
        let before = bx(|| store.get(&run)).await.unwrap();
        let revision = before.revision - u64::from(stale);
        assert_eq!(
            bx(|| store.resume_held(&run, revision, command(key), now)).await,
            Err(expected_error)
        );
        assert_eq!(bx(|| store.get(&run)).await.unwrap(), before);
        assert_eq!(bx(|| store.owner_receipt(key)).await.unwrap(), None);
    }
}

/// Concurrent replays have one effect; receipt survives progress, rehold and cancellation.
pub async fn resume_receipts<S: ControlStore + 'static>(store: Arc<S>, reopened: Arc<S>) {
    let run = held(&*store, "receipts", None).await;
    let before = bx(|| store.get(&run)).await.unwrap();
    let revision = before.revision;
    let tasks: Vec<_> = (0..12)
        .map(|i| {
            let store = if i % 2 == 0 {
                store.clone()
            } else {
                reopened.clone()
            };
            let run = run.clone();
            tokio::spawn(async move {
                bx(|| store.resume_held(&run, revision, command("resume"), 10 + i))
                    .await
                    .unwrap()
            })
        })
        .collect();
    let mut receipts = vec![];
    for task in tasks {
        receipts.push(task.await.unwrap());
    }
    assert!(receipts.windows(2).all(|p| p[0] == p[1]));
    let receipt = receipts.remove(0);
    assert_eq!(
        receipt.result,
        OwnerResult::Resumed {
            revision: revision + 1
        }
    );
    assert_eq!(bx(|| store.get(&run)).await.unwrap().revision, revision + 1);
    assert_eq!(
        bx(|| reopened.owner_receipt("resume")).await.unwrap(),
        Some(receipt.clone())
    );
    let mut different = command("resume");
    different.request_digest = "different".into();
    assert_eq!(
        bx(|| store.resume_held(&run, revision, different, 50)).await,
        Err(Error::Conflict)
    );
    // Even a caller incorrectly reusing the digest cannot retarget the typed request.
    assert_eq!(
        bx(|| store.resume_held(&run, revision + 1, command("resume"), 50)).await,
        Err(Error::Conflict)
    );
    let claim = bx(|| store.claim(&run, "later", 100, 10)).await.unwrap();
    bx(|| store.wait(&claim, OPERATOR_ACTION_WAIT, 100))
        .await
        .unwrap();
    let held_again = bx(|| store.get(&run)).await.unwrap();
    assert_eq!(
        bx(|| reopened.resume_held(&run, revision, command("resume"), 200))
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(bx(|| store.get(&run)).await.unwrap(), held_again);
    bx(|| store.cancel(&run, "later cancellation"))
        .await
        .unwrap();
    let cancelled = bx(|| store.get(&run)).await.unwrap();
    assert_eq!(
        bx(|| reopened.resume_held(&run, revision, command("resume"), 300))
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(
        bx(|| reopened.owner_receipt("resume")).await.unwrap(),
        Some(receipt)
    );
    assert_eq!(bx(|| store.get(&run)).await.unwrap(), cancelled);
}

/// All ancestor accounts participate, including unknown and fully reserved capacity.
pub async fn resume_budget<S: ControlStore>(store: Arc<S>) {
    for (key, unknown, used, quantity, expected) in [
        ("reserved", false, false, false, Error::BudgetAtLimit),
        ("unknown", true, false, false, Error::UsageUnknown),
        ("used", false, true, false, Error::BudgetAtLimit),
        ("quantity", false, false, true, Error::BudgetAtLimit),
    ] {
        let policy = AccountPolicy {
            quantity_limits: if quantity {
                [("jobs".into(), 1)].into()
            } else {
                Default::default()
            },
            ..Default::default()
        };
        let parent =
            bx(|| store.create_account_with_policy("owner", Amount::new(100, 100), None, policy))
                .await
                .unwrap();
        let child =
            bx(|| store.create_account("owner", Amount::new(200, 200), Some(parent.clone())))
                .await
                .unwrap();
        let run = bx(|| store.start(RunSpec::new("owner", key, child, "test")))
            .await
            .unwrap();
        let claim = bx(|| store.claim(&run, "worker", 0, 10)).await.unwrap();
        let mut req = DispatchRequest::new(
            key,
            OperationKind::Tool,
            "test",
            json!({}),
            if quantity {
                Amount::new(1, 1)
            } else {
                Amount::new(100, 100)
            },
        );
        if quantity {
            req.quantities.insert("jobs".into(), 1);
        }
        let op = bx(|| store.begin(&claim, req, 0)).await.unwrap();
        if unknown {
            bx(|| store.record(&claim, &op.id, ExternalResult::Unknown, 0))
                .await
                .unwrap();
        }
        if used {
            bx(|| {
                store.record(
                    &claim,
                    &op.id,
                    ExternalResult::Applied {
                        output: json!({}),
                        usage: Usage::measured(100, 0, 100),
                        external_id: None,
                        physical_evidence: None,
                    },
                    0,
                )
            })
            .await
            .unwrap();
        }
        bx(|| store.wait(&claim, OPERATOR_ACTION_WAIT, 0))
            .await
            .unwrap();
        let before = bx(|| store.get(&run)).await.unwrap();
        let balance = bx(|| store.account(&parent)).await.unwrap();
        assert_eq!(
            bx(|| store.resume_held(&run, before.revision, command(key), 10)).await,
            Err(expected)
        );
        assert_eq!(bx(|| store.get(&run)).await.unwrap(), before);
        assert_eq!(bx(|| store.account(&parent)).await.unwrap(), balance);
        assert_eq!(bx(|| store.owner_receipt(key)).await.unwrap(), None);
    }
}

pub async fn unsettled<S: ControlStore>(store: &S, key: &str) -> (RunHandle, String) {
    let account = bx(|| store.create_account("owner", Amount::new(100_000, 1_000_000), None))
        .await
        .unwrap();
    let run = bx(|| store.start(RunSpec::new("owner", key, account, "test")))
        .await
        .unwrap();
    let claim = bx(|| store.claim(&run, "worker", 0, 10)).await.unwrap();
    let mut request = DispatchRequest::new(
        "image",
        OperationKind::Tool,
        "draw",
        json!({}),
        Amount::new(1_000, 50_000),
    );
    request.price = Some(settlement_contracts::rates());
    let op = bx(|| store.begin(&claim, request, 0)).await.unwrap();
    assert_eq!(
        bx(|| store.record(
            &claim,
            &op.id,
            ExternalResult::Applied {
                output: json!({"saved":true}),
                usage: settlement_contracts::stored_usage(),
                external_id: None,
                physical_evidence: None
            },
            0
        ))
        .await,
        Err(Error::UsageUnknown)
    );
    bx(|| store.wait(&claim, OPERATOR_ACTION_WAIT, 0))
        .await
        .unwrap();
    (run, op.id)
}

pub async fn settlement_receipts<S: ControlStore + 'static>(store: Arc<S>, reopened: Arc<S>) {
    let (run, op) = unsettled(&*store, "settlement-receipts").await;
    let before = bx(|| store.get(&run)).await.unwrap();
    // With no live claim, an unresolved saved result alone still refuses resume.
    assert_eq!(
        bx(|| store.resume_held(&run, before.revision, command("unsettled-resume"), 10)).await,
        Err(Error::Conflict)
    );
    assert_eq!(
        bx(|| store.owner_receipt("unsettled-resume"))
            .await
            .unwrap(),
        None
    );
    let tasks: Vec<_> = (0..12)
        .map(|i| {
            let store = if i % 2 == 0 {
                store.clone()
            } else {
                reopened.clone()
            };
            let (run, op) = (run.clone(), op.clone());
            tokio::spawn(async move {
                bx(|| {
                    store.settle_recovered(
                        &run,
                        &op,
                        settlement_contracts::recovery(),
                        command("settle"),
                        10 + i,
                    )
                })
                .await
                .unwrap()
            })
        })
        .collect();
    let mut receipts = vec![];
    for task in tasks {
        receipts.push(task.await.unwrap());
    }
    assert!(receipts.windows(2).all(|p| p[0] == p[1]));
    let receipt = receipts.remove(0);
    let OwnerResult::Settled(ref record) = receipt.result else {
        panic!("settlement required")
    };
    assert_eq!(
        record.amount,
        Amount::new(944, settlement_contracts::EXPECTED_MICRO_USD)
    );
    let after = bx(|| store.get(&run)).await.unwrap();
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(after.state, RunState::Waiting(OPERATOR_ACTION_WAIT.into()));
    let account = bx(|| store.account(&after.spec.account)).await.unwrap();
    assert_eq!(account.used, record.amount);
    assert_eq!(account.reserved, Amount::default());
    assert_eq!(account.unknown, 0);
    let mut different = command("settle");
    different.request_digest = "different".into();
    assert_eq!(
        bx(|| store.settle_recovered(&run, &op, settlement_contracts::recovery(), different, 50))
            .await,
        Err(Error::Conflict)
    );
    assert_eq!(
        bx(|| store.settle_recovered(
            &run,
            &op,
            settlement_contracts::recovery(),
            command("other-settle"),
            50
        ))
        .await,
        Err(Error::Conflict)
    );
    assert_eq!(
        bx(|| store.owner_receipt("other-settle")).await.unwrap(),
        None
    );
    assert_eq!(
        bx(|| store.resume_held(&run, after.revision, command("settle"), 50)).await,
        Err(Error::Conflict)
    );
    bx(|| store.resume_held(&run, after.revision, command("after-settle"), 50))
        .await
        .unwrap();
    let claim = bx(|| store.claim(&run, "later", 50, 10)).await.unwrap();
    bx(|| store.wait(&claim, OPERATOR_ACTION_WAIT, 50))
        .await
        .unwrap();
    assert_eq!(
        bx(|| reopened.owner_receipt("settle")).await.unwrap(),
        Some(receipt.clone())
    );
    bx(|| store.cancel(&run, "later")).await.unwrap();
    let cancelled = bx(|| store.get(&run)).await.unwrap();
    assert_eq!(
        bx(|| reopened.settle_recovered(
            &run,
            &op,
            settlement_contracts::recovery(),
            command("settle"),
            100
        ))
        .await
        .unwrap(),
        receipt
    );
    assert_eq!(bx(|| store.get(&run)).await.unwrap(), cancelled);
    assert_eq!(
        bx(|| store.account(&after.spec.account)).await.unwrap(),
        account
    );
}
