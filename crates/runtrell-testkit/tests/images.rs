// SPDX-License-Identifier: Apache-2.0
use runtrell::{data::*, memory::MemoryStore};
use runtrell_testkit::{block_on, image_contracts};
use std::sync::Arc;
#[test]
fn images_normal_loop_and_saved_result_recovery() {
    block_on(image_contracts::workflow(
        Arc::new(MemoryStore::default()),
        DataStore::memory(StoragePolicy::default()),
    ));
}
#[test]
fn images_paid_validation_failure_and_unknown_usage() {
    block_on(image_contracts::billed_failures(Arc::new(
        MemoryStore::default(),
    )));
}
