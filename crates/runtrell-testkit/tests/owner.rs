// SPDX-License-Identifier: Apache-2.0
use runtrell::memory::MemoryStore;
use runtrell::test_outcomes::{ExpectedOutcome, OutcomeException, expect, expect_operator_wait};
use runtrell_testkit::{block_on, owner_contracts};
use std::sync::Arc;

#[test]
fn cancelled_resume_is_refused() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(owner_contracts::cancelled_resume_is_refused(Arc::new(
        MemoryStore::default(),
    )));
}

#[test]
fn resume_preconditions() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(owner_contracts::resume_preconditions(Arc::new(
        MemoryStore::default(),
    )));
}
#[tokio::test]
async fn resume_receipts() {
    let _outcomes = runtrell::test_outcomes::guard();
    let store = Arc::new(MemoryStore::default());
    owner_contracts::resume_receipts(store.clone(), store).await;
}

#[tokio::test]
async fn settlement_receipts() {
    let _outcomes = runtrell::test_outcomes::guard();
    let store = Arc::new(MemoryStore::default());
    owner_contracts::settlement_receipts(store.clone(), store).await;
}

#[tokio::test]
async fn failed_write_has_no_effect_or_receipt() {
    let _outcomes = runtrell::test_outcomes::guard();
    use runtrell::*;
    let store = MemoryStore::default();
    let run = owner_contracts::held(&store, "failure", None).await;
    let before = store.get(&run).await.unwrap();
    store.fail_next_write();
    assert_eq!(
        store
            .resume_held(
                &run,
                before.revision,
                owner_contracts::command("failure"),
                10
            )
            .await,
        Err(Error::Storage)
    );
    assert_eq!(store.get(&run).await.unwrap(), before);
    assert_eq!(store.owner_receipt("failure").await.unwrap(), None);
    expect_operator_wait(&run);
    let (run, operation) = owner_contracts::unsettled(&store, "failed-settle").await;
    let before = store.get(&run).await.unwrap();
    let account = store.account(&before.spec.account).await.unwrap();
    store.fail_next_write();
    assert_eq!(
        store
            .settle_recovered(
                &run,
                &operation,
                runtrell_testkit::settlement_contracts::recovery(),
                owner_contracts::command("failed-settle"),
                10
            )
            .await,
        Err(Error::Storage)
    );
    assert_eq!(store.get(&run).await.unwrap(), before);
    assert_eq!(store.account(&before.spec.account).await.unwrap(), account);
    assert_eq!(store.owner_receipt("failed-settle").await.unwrap(), None);
    expect(
        &run,
        ExpectedOutcome {
            exceptions: vec![OutcomeException {
                operator_action: true,
                ..OutcomeException::unresolved_usage(
                    "image",
                    OperationKind::Tool,
                    OperationState::Applied,
                    UsageIssue::PricingUnavailable,
                )
            }],
            operator_wait: true,
            ..Default::default()
        },
    );
}
#[test]
fn resume_budget() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(owner_contracts::resume_budget(Arc::new(
        MemoryStore::default(),
    )));
}
