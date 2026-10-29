// SPDX-License-Identifier: Apache-2.0
mod support;
#[test]
fn images_normal_loop_and_saved_result_recovery() {
    support::block_on(runtrell_testkit::image_contracts::workflow(
        std::sync::Arc::new(support::TestPg::default()),
        runtrell::data::DataStore::memory(runtrell::data::StoragePolicy::default()),
    ));
}
#[test]
fn images_paid_validation_failure_and_unknown_usage() {
    support::block_on(runtrell_testkit::image_contracts::billed_failures(
        std::sync::Arc::new(support::TestPg::default()),
    ));
}
macro_rules! suite {
    () => {
        mod runtime {
            include!("suite/runtime.rs");
        }
        mod fences {
            include!("suite/fences.rs");
        }
        mod recovery {
            include!("suite/recovery.rs");
        }
        mod retry {
            include!("suite/retry.rs");
        }
    };
}
mod memory {
    use super::support::block_on;
    use runtrell::memory::MemoryStore;
    use runtrell_testkit::runtime;
    suite!();
}
mod postgres {
    use super::support::{TestPg as MemoryStore, block_on};
    use runtrell::*;
    use runtrell_testkit::*;
    use std::sync::Arc;
    fn runtime(
        store: Arc<MemoryStore>,
        model: FakeModel,
        tools: Vec<BusinessTool>,
    ) -> Runtime<
        MemoryStore,
        FakeModel,
        BusinessTool,
        FakePolicy,
        TestClock,
        DecisionValidator,
        DecisionFinalizer,
    > {
        Runtime {
            store,
            model,
            tools,
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: std::sync::Arc::new(DecisionValidator::default()),
            finalizer: std::sync::Arc::new(DecisionFinalizer),
        }
    }
    suite!();
}

#[test]
fn selectable_account_policies() {
    support::block_on(runtrell_testkit::budget_contracts::policies(
        &support::TestPg::default(),
    ));
}
#[test]
fn budget_settlement_finalization_and_billed_attempts() {
    support::block_on(
        runtrell_testkit::budget_contracts::settlement_and_finalization(&support::TestPg::default()),
    );
}
#[test]
fn run_reservation_policies() {
    support::block_on(runtrell_testkit::budget_contracts::driver_policies(
        std::sync::Arc::new(support::TestPg::default()),
    ));
}
#[test]
fn host_priority_preserves_tenant_rotation() {
    support::block_on(runtrell_testkit::budget_contracts::scheduling(
        &support::TestPg::default(),
    ));
}
#[test]
fn selectable_axes_and_ancestor_quantity_guarantees() {
    support::block_on(runtrell_testkit::budget_contracts::axes(
        &support::TestPg::default(),
    ));
}
#[test]
fn tokenless_paid_tool_reserves_host_quantity() {
    support::block_on(runtrell_testkit::budget_contracts::quantity_tool(
        std::sync::Arc::new(support::TestPg::default()),
    ));
}
#[test]
fn model_quote_race() {
    support::block_on(runtrell_testkit::budget_contracts::model_quote_race(
        std::sync::Arc::new(support::TestPg::default()),
    ));
}
#[test]
fn preparation_and_physical_evidence() {
    support::block_on(
        runtrell_testkit::recovery_contracts::preparation_and_evidence(std::sync::Arc::new(
            support::TestPg::default(),
        )),
    );
}
#[test]
fn external_job_preparation() {
    support::block_on(runtrell_testkit::recovery_contracts::job_preparation(
        std::sync::Arc::new(support::TestPg::default()),
    ));
}

#[test]
fn json_artifact_syntax_repairs_with_new_runtime() {
    support::block_on(
        runtrell_testkit::repair_contracts::json_artifact_repair_contract(std::sync::Arc::new(
            support::TestPg::default(),
        )),
    );
}

#[test]
fn validation_no_progress_survives_resume() {
    support::block_on(runtrell_testkit::repair_contracts::no_progress_contract(
        std::sync::Arc::new(support::TestPg::default()),
    ));
}
