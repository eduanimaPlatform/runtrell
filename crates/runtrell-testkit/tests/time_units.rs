// SPDX-License-Identifier: Apache-2.0
//! Millisecond time contracts against the in-memory store; fake clock only.
use runtrell::memory::MemoryStore;
use runtrell_testkit::*;
use std::sync::Arc;

#[test]
fn lease_and_deadline_boundaries() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(time_contracts::lease_and_deadline_boundaries(Arc::new(
        MemoryStore::default(),
    )));
}
#[test]
fn lease_overflow_is_refused() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(time_contracts::lease_overflow_is_refused(Arc::new(
        MemoryStore::default(),
    )));
}
#[test]
fn operation_deadline_is_now_plus_timeout_ms() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(time_contracts::operation_deadline_is_now_plus_timeout_ms(
        Arc::new(MemoryStore::default()),
    ));
}
#[test]
fn capacity_wait_default_is_sixty_seconds_in_ms() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(
        time_contracts::capacity_wait_default_is_sixty_seconds_in_ms(Arc::new(MemoryStore::new(
            time_contracts::one_model_slot(),
        ))),
    );
}

#[test]
fn image_usage_operator_recovery() {
    let _outcomes = runtrell::test_outcomes::guard();
    let store = Arc::new(MemoryStore::default());
    block_on(settlement_contracts::image_usage_operator_recovery(
        store.clone(),
        store,
    ));
}
#[test]
fn partial_price_evidence_recovery() {
    let _outcomes = runtrell::test_outcomes::guard();
    let store = Arc::new(MemoryStore::default());
    block_on(settlement_contracts::partial_price_evidence_recovery(
        store.clone(),
        store,
    ));
}
#[test]
fn wait_follows_the_lookup_window() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(settlement_contracts::wait_follows_the_lookup_window(
        Arc::new(MemoryStore::default()),
    ));
}
#[test]
fn lost_reply_keeps_lookup_recovery() {
    let _outcomes = runtrell::test_outcomes::guard();
    let store = Arc::new(MemoryStore::default());
    block_on(settlement_contracts::lost_reply_keeps_lookup_recovery(
        store.clone(),
        store,
    ));
}
#[test]
fn settle_never_resumes() {
    let _outcomes = runtrell::test_outcomes::guard();
    let store = Arc::new(MemoryStore::default());
    block_on(settlement_contracts::settle_never_resumes(
        store.clone(),
        store,
    ));
}

#[test]
fn shared_allowance_consumption_is_atomic_in_memory() {
    let _outcomes = runtrell::test_outcomes::guard();
    let first = MemoryStore::default();
    let second = first.clone();
    block_on(settlement_contracts::shared_allowance(&first, &second));
}

#[test]
fn managed_allowance_settles_atomically_in_memory() {
    let _outcomes = runtrell::test_outcomes::guard();
    let store = MemoryStore::default();
    block_on(settlement_contracts::managed_allowance_settlement(
        &store,
        &store.clone(),
        || store.fail_next_write(),
    ));
}

#[test]
fn dependent_resource_extension_in_memory() {
    let _outcomes = runtrell::test_outcomes::guard();
    let store = Arc::new(MemoryStore::default());
    block_on(settlement_contracts::dependent_resource_extension(
        store.clone(),
        store,
    ));
}
