// SPDX-License-Identifier: Apache-2.0
// Shared contract fixture from runtrell-2a testkit (2d31acf).
use super::{MemoryStore, block_on};
use runtrell::*;
use runtrell_testkit::*;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
struct RetryModel {
    calls: AtomicU64,
    success: bool,
}
impl ModelPort for RetryModel {
    fn limits(&self) -> ModelLimits {
        FakeModel::default().limits()
    }
    fn input_upper_bound(&self, r: &ModelRequest) -> Result<u64> {
        FakeModel::default().input_upper_bound(r)
    }
    fn recovery(&self) -> Option<(String, u64)> {
        Some(("retry".into(), 100))
    }
    async fn lookup(&self, _: &ToolContext, _: ModelRequest) -> Result<Lookup> {
        Ok(Lookup::Unknown)
    }
    async fn invoke(&self, _: &ToolContext, _: ModelRequest) -> Result<ExternalResult> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let mut usage = Usage::measured(3, 1, 4);
        usage.model = "fake".into();
        usage.cached = Some(0);
        if n == 0 || !self.success {
            Ok(ExternalResult::Failed {
                code: "TRANSIENT".into(),
                usage,
                retry_after: Some(1),
            })
        } else {
            Ok(ExternalResult::Applied {
                output: serde_json::to_value(FakeModel::decision(json!({"decision":"answered"})))
                    .unwrap(),
                usage,
                external_id: None,
            })
        }
    }
}
#[test]
fn charged_retry_is_settled_before_next_attempt_and_uses_shared_budget() {
    block_on(async {
        let s = Arc::new(MemoryStore::default());
        let a = s
            .create_account("t", Amount::new(100, 100), None)
            .await
            .unwrap();
        let h = s
            .start(RunSpec::new("t", "retry", a.clone(), "answer"))
            .await
            .unwrap();
        let c = s.claim(&h, "worker", 0, 100).await.unwrap();
        let clock = TestClock::default();
        let policy = FakePolicy::default();
        let rt = Runtime {
            store: s.clone(),
            model: RetryModel {
                calls: AtomicU64::new(0),
                success: true,
            },
            tools: Vec::<BusinessTool>::new(),
            policy,
            clock: clock.clone(),
            validator: DecisionValidator::default(),
            finalizer: DecisionFinalizer,
        };
        assert!(matches!(
            rt.drive(&c).await.unwrap().state,
            RunState::Waiting(_)
        ));
        assert_eq!(s.account(&a).await.unwrap().used.tokens, 4);
        assert_eq!(s.account(&a).await.unwrap().reserved.tokens, 0);
        assert!(matches!(
            rt.drive(&c).await.unwrap().state,
            RunState::Waiting(_)
        ));
        assert_eq!(rt.model.calls.load(Ordering::SeqCst), 1);
        clock.set(1);
        assert_eq!(
            rt.drive(&c).await.unwrap().state,
            RunState::Terminal(Terminal::Succeeded)
        );
        assert_eq!(s.account(&a).await.unwrap().used.tokens, 8);
        assert_eq!(s.operations(&h).await.unwrap().len(), 2);
    });
}
#[test]
fn retry_bound_and_authorization_are_rechecked_after_restart() {
    block_on(async {
        for revoke in [false, true] {
            let s = Arc::new(MemoryStore::default());
            let a = s
                .create_account("t", Amount::new(1000, 1000), None)
                .await
                .unwrap();
            let h = s
                .start(RunSpec::new("t", "retry", a, "answer"))
                .await
                .unwrap();
            let c = s.claim(&h, "worker", 0, 100).await.unwrap();
            let clock = TestClock::default();
            let policy = FakePolicy::default();
            let rt = Runtime {
                store: s.clone(),
                model: RetryModel {
                    calls: AtomicU64::new(0),
                    success: false,
                },
                tools: Vec::<BusinessTool>::new(),
                policy: policy.clone(),
                clock: clock.clone(),
                validator: DecisionValidator::default(),
                finalizer: DecisionFinalizer,
            };
            rt.drive(&c).await.unwrap();
            if revoke {
                policy.revoked.store(true, Ordering::SeqCst);
            }
            for time in 1..=3 {
                clock.set(time);
                rt.drive(&c).await.unwrap();
            }
            assert_eq!(
                rt.model.calls.load(Ordering::SeqCst),
                if revoke { 1 } else { 3 }
            );
            assert!(matches!(
                s.get(&h).await.unwrap().state,
                RunState::Terminal(Terminal::Failed(_))
            ));
        }
    });
}
