// SPDX-License-Identifier: Apache-2.0
use runtrell::{data::*, memory::MemoryStore};
use runtrell_testkit::{driver_contracts, image_contracts};
use std::sync::Arc;

fn small_stack(run: impl FnOnce() + Send + 'static) {
    // Half Tokio's default 2 MiB worker stack. Poll on this thread rather
    // than handing the driver to a worker with an unconstrained stack.
    std::thread::Builder::new()
        .name("driver-small-stack".into())
        .stack_size(1024 * 1024)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn model_tool_compaction_and_recovery_on_small_stack() {
    small_stack(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let store = Arc::new(MemoryStore::default());
        runtime.block_on(Box::pin(driver_contracts::automatic_compaction(
            store.clone(),
            store,
        )));
    });
}

#[test]
fn image_attachments_and_saved_result_recovery_on_small_stack() {
    small_stack(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(Box::pin(image_contracts::workflow(
            Arc::new(MemoryStore::default()),
            DataStore::memory(StoragePolicy::default()),
        )));
    });
}
