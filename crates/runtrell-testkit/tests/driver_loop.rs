// SPDX-License-Identifier: Apache-2.0
use runtrell::memory::MemoryStore;
use runtrell_testkit::*;
use std::sync::Arc;

#[test]
fn max_tokens_continues_with_new_reservation() {
    block_on(driver_contracts::token_continuation(Arc::new(
        MemoryStore::default(),
    )));
}
#[test]
fn continuation_rechecks_deadline_after_settlement() {
    block_on(driver_contracts::continuation_deadline(Arc::new(
        MemoryStore::default(),
    )));
}
#[test]
fn continuation_obeys_host_limit_budget_and_shrink_without_default_count() {
    block_on(driver_contracts::continuation_stops(Arc::new(
        MemoryStore::default(),
    )));
}
#[test]
fn automatic_context_compaction_resumes_from_committed_summary() {
    let store = Arc::new(MemoryStore::default());
    block_on(driver_contracts::automatic_compaction(store.clone(), store));
}
#[test]
fn host_receipt_classifies_repairs_and_keeps_infrastructure_errors_separate() {
    block_on(driver_contracts::receipt_classification(Arc::new(
        MemoryStore::default(),
    )));
}
#[test]
fn tool_schema_diagnostics_obey_explicit_repair_limit() {
    block_on(driver_contracts::tool_repairs(Arc::new(
        MemoryStore::default(),
    )));
}
