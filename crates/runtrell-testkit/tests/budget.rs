// SPDX-License-Identifier: Apache-2.0
use runtrell_testkit::{block_on, budget_contracts};
#[test]
fn selectable_account_policies() {
    block_on(budget_contracts::policies(
        &runtrell::memory::MemoryStore::default(),
    ));
}
#[test]
fn image_price_and_unknown_measurements() {
    budget_contracts::image_price();
}
#[test]
fn settlement_finalization_and_billed_attempts() {
    block_on(budget_contracts::settlement_and_finalization(
        &runtrell::memory::MemoryStore::default(),
    ));
}
#[test]
fn run_reservation_policies() {
    block_on(budget_contracts::driver_policies(std::sync::Arc::new(
        runtrell::memory::MemoryStore::default(),
    )));
}
#[test]
fn host_priority_preserves_tenant_rotation() {
    block_on(budget_contracts::scheduling(
        &runtrell::memory::MemoryStore::default(),
    ));
}
#[test]
fn selectable_axes_and_ancestor_quantity_guarantees() {
    block_on(budget_contracts::axes(
        &runtrell::memory::MemoryStore::default(),
    ));
}
#[test]
fn tokenless_paid_tool_reserves_host_quantity() {
    block_on(budget_contracts::quantity_tool(std::sync::Arc::new(
        runtrell::memory::MemoryStore::default(),
    )));
}
