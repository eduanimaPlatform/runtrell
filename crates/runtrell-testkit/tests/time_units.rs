// SPDX-License-Identifier: Apache-2.0
//! Millisecond time contracts against the in-memory store; fake clock only.
use runtrell::memory::MemoryStore;
use runtrell_testkit::*;
use std::sync::Arc;

#[test]
fn lease_and_deadline_boundaries() {
    block_on(time_contracts::lease_and_deadline_boundaries(Arc::new(
        MemoryStore::default(),
    )));
}
#[test]
fn lease_overflow_is_refused() {
    block_on(time_contracts::lease_overflow_is_refused(Arc::new(
        MemoryStore::default(),
    )));
}
#[test]
fn operation_deadline_is_now_plus_timeout_ms() {
    block_on(time_contracts::operation_deadline_is_now_plus_timeout_ms(
        Arc::new(MemoryStore::default()),
    ));
}
#[test]
fn capacity_wait_default_is_sixty_seconds_in_ms() {
    block_on(
        time_contracts::capacity_wait_default_is_sixty_seconds_in_ms(Arc::new(MemoryStore::new(
            time_contracts::one_model_slot(),
        ))),
    );
}

#[test]
fn image_usage_operator_recovery() {
    let store = Arc::new(MemoryStore::default());
    block_on(settlement_contracts::image_usage_operator_recovery(
        store.clone(),
        store,
    ));
}
#[test]
fn partial_price_evidence_recovery() {
    let store = Arc::new(MemoryStore::default());
    block_on(settlement_contracts::partial_price_evidence_recovery(
        store.clone(),
        store,
    ));
}
#[test]
fn wait_follows_the_lookup_window() {
    block_on(settlement_contracts::wait_follows_the_lookup_window(
        Arc::new(MemoryStore::default()),
    ));
}
#[test]
fn lost_reply_keeps_lookup_recovery() {
    let store = Arc::new(MemoryStore::default());
    block_on(settlement_contracts::lost_reply_keeps_lookup_recovery(
        store.clone(),
        store,
    ));
}
#[test]
fn settle_never_resumes() {
    let store = Arc::new(MemoryStore::default());
    block_on(settlement_contracts::settle_never_resumes(
        store.clone(),
        store,
    ));
}
