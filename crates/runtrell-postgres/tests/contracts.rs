// SPDX-License-Identifier: Apache-2.0
mod support;
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
            validator: DecisionValidator::default(),
            finalizer: DecisionFinalizer,
        }
    }
    suite!();
}
