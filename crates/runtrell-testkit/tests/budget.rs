// SPDX-License-Identifier: Apache-2.0
use runtrell_testkit::{block_on, budget_contracts};
#[test]
fn selectable_account_policies() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(budget_contracts::policies(
        &runtrell::memory::MemoryStore::default(),
    ));
}
#[test]
fn image_price_and_unknown_measurements() {
    let _outcomes = runtrell::test_outcomes::guard();
    budget_contracts::image_price();
}
#[test]
fn settlement_finalization_and_billed_attempts() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(budget_contracts::settlement_and_finalization(
        &runtrell::memory::MemoryStore::default(),
    ));
}
#[test]
fn run_reservation_policies() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(budget_contracts::driver_policies(std::sync::Arc::new(
        runtrell::memory::MemoryStore::default(),
    )));
}
#[test]
fn host_priority_preserves_tenant_rotation() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(budget_contracts::scheduling(
        &runtrell::memory::MemoryStore::default(),
    ));
}
#[test]
fn selectable_axes_and_ancestor_quantity_guarantees() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(budget_contracts::axes(
        &runtrell::memory::MemoryStore::default(),
    ));
}
#[test]
fn tokenless_paid_tool_reserves_host_quantity() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(budget_contracts::quantity_tool(std::sync::Arc::new(
        runtrell::memory::MemoryStore::default(),
    )));
}
#[test]
fn model_quote_race() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(budget_contracts::model_quote_race(std::sync::Arc::new(
        runtrell::memory::MemoryStore::default(),
    )));
}
#[test]
fn preparation_and_physical_evidence() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(
        runtrell_testkit::recovery_contracts::preparation_and_evidence(std::sync::Arc::new(
            runtrell::memory::MemoryStore::default(),
        )),
    );
}
#[test]
fn external_job_preparation() {
    let _outcomes = runtrell::test_outcomes::guard();
    block_on(runtrell_testkit::recovery_contracts::job_preparation(
        std::sync::Arc::new(runtrell::memory::MemoryStore::default()),
    ));
}
