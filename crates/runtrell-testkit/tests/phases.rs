// SPDX-License-Identifier: Apache-2.0
use runtrell::memory::MemoryStore;
use runtrell_testkit::{block_on, phase_contracts};
use std::sync::Arc;

#[test]
fn intermediate_completion_reopens_only_after_purge() {
    let _outcomes = runtrell::test_outcomes::guard();
    let store = Arc::new(MemoryStore::default());
    block_on(
        phase_contracts::intermediate_completion_reopens_only_after_purge(store.clone(), store),
    );
}

#[test]
fn children_are_bound_to_the_open_phase() {
    let _outcomes = runtrell::test_outcomes::guard();
    let store = Arc::new(MemoryStore::default());
    block_on(phase_contracts::children_are_bound_to_the_open_phase(
        store.clone(),
        store,
    ));
}
