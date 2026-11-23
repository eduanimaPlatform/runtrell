// SPDX-License-Identifier: Apache-2.0
use runtrell::{data::*, memory::MemoryStore};
use runtrell_testkit::{block_on, image_contracts};
use std::sync::Arc;
#[test]
fn images_normal_loop_and_saved_result_recovery() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(image_contracts::workflow(
        Arc::new(MemoryStore::default()),
        DataStore::memory(StoragePolicy::default()),
    ));
}
#[test]
fn images_paid_validation_failure_and_unknown_usage() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(image_contracts::billed_failures(Arc::new(
        MemoryStore::default(),
    )));
}
#[test]
fn image_size_repair_returns_to_agent_without_a_bad_send() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(image_contracts::size_repair_returns_to_agent());
}
#[test]
fn image_size_repair_stops_at_repair_limit() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(image_contracts::size_repair_respects_limit());
}
