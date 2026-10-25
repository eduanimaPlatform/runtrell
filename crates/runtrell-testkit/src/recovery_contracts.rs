// SPDX-License-Identifier: Apache-2.0
//! Durable preparation and adapter-reported physical-send evidence contracts.
use runtrell::*;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

struct PreparedModel {
    fail: AtomicBool,
    prepared: AtomicUsize,
    expires: AtomicU64,
    found: AtomicBool,
    unsent: AtomicBool,
    seen: Mutex<Vec<RecoveryProof>>,
    generation: Mutex<String>,
}
impl ModelPort for PreparedModel {
    fn limits(&self) -> ModelLimits {
        crate::FakeModel::scripted(vec![]).limits()
    }
    fn input_upper_bound(&self, _: &ModelRequest) -> Result<u64> {
        Ok(5)
    }
    fn recovery(&self) -> Option<(String, u64)> {
        None
    }
    fn requires_recovery_preparation(&self) -> bool {
        true
    }
    async fn prepare_recovery(
        &self,
        c: &PreparationContext,
        _: &Value,
    ) -> Result<Option<RecoveryProof>> {
        self.prepared.fetch_add(1, Ordering::SeqCst);
        tokio::task::yield_now().await;
        assert_eq!(c.principal, "host");
        assert!(c.attempt >= 1);
        assert!(!c.key.is_empty());
        if self.fail.load(Ordering::SeqCst) {
            return Err(Error::Storage);
        }
        Ok(Some(RecoveryProof {
            bytes: vec![1, 2, 3],
            generation: self.generation.lock().unwrap().clone(),
            expires_at: self.expires.load(Ordering::SeqCst),
        }))
    }
    async fn invoke(&self, c: &ToolContext, _: ModelRequest) -> Result<ExternalResult> {
        self.seen
            .lock()
            .unwrap()
            .push(c.recovery_proof.clone().unwrap());
        Ok(ExternalResult::Unknown)
    }
    async fn lookup(&self, c: &ToolContext, _: ModelRequest) -> Result<Lookup> {
        assert_eq!(
            Some(&self.seen.lock().unwrap()[0]),
            c.recovery_proof.as_ref()
        );
        if c.recovery_proof.as_ref().unwrap().generation != *self.generation.lock().unwrap() {
            return Ok(Lookup::Unknown);
        }
        if self.unsent.load(Ordering::SeqCst) {
            return Ok(Lookup::NotFound {
                scope: c.recovery_proof.as_ref().unwrap().generation.clone(),
            });
        }
        if self.found.load(Ordering::SeqCst) {
            Ok(Lookup::Found(ExternalResult::Failed {
                physical_evidence: Some(evidence()),
                code: "provider-failed".into(),
                usage: Usage::measured(0, 0, 2),
                retry_after: Some(1),
            }))
        } else {
            Ok(Lookup::Unknown)
        }
    }
}
fn evidence() -> PhysicalSendEvidence {
    PhysicalSendEvidence {
        completeness: EvidenceCompleteness::Truncated,
        observations: vec![
            PhysicalSendObservation {
                sequence: 1,
                bytes: vec![9, 8],
            },
            PhysicalSendObservation {
                sequence: 3,
                bytes: vec![7],
            },
        ],
    }
}
pub async fn preparation_and_evidence<S: ControlStore>(s: Arc<S>) {
    let account = s
        .create_account("t", Amount::new(100, 100), None)
        .await
        .unwrap();
    let h = s
        .start(RunSpec::new("t", "preparation", account.clone(), "judge"))
        .await
        .unwrap();
    let c = s.claim(&h, "w", 0, 100).await.unwrap();
    let rt = Runtime {
        store: s.clone(),
        model: PreparedModel {
            fail: AtomicBool::new(true),
            prepared: AtomicUsize::new(0),
            expires: AtomicU64::new(50),
            found: AtomicBool::new(false),
            unsent: AtomicBool::new(false),
            seen: Mutex::new(vec![]),
            generation: Mutex::new("epoch-a".into()),
        },
        tools: Vec::<crate::BusinessTool>::new(),
        policy: crate::FakePolicy::default(),
        clock: crate::TestClock::default(),
        validator: Arc::new(crate::DecisionValidator::default()),
        finalizer: Arc::new(crate::DecisionFinalizer),
    };
    let request = ModelRequest {
        model: "fake".into(),
        messages: vec![],
        tools: vec![],
        max_output: 5,
        metadata: Value::Null,
    };
    let d = DispatchRequest::new(
        "prepared",
        OperationKind::Model,
        "fake",
        json!(request),
        Amount::new(10, 10),
    );
    macro_rules! dispatch {
        () => {
            rt.dispatch(
                &c,
                d.clone(),
                "model",
                |ctx| {
                    let request = request.clone();
                    let model = &rt.model;
                    async move { model.invoke(&ctx, request).await }
                },
                |ctx| {
                    let request = request.clone();
                    let model = &rt.model;
                    async move { model.lookup(&ctx, request).await }
                },
            )
            .await
        };
    }
    assert_eq!(dispatch!(), Err(Error::Storage));
    assert!(s.list_operations(&h).await.unwrap().is_empty());
    assert_eq!(
        s.account(&account).await.unwrap().reserved,
        Amount::default()
    );
    assert!(rt.model.seen.lock().unwrap().is_empty());
    rt.model.fail.store(false, Ordering::SeqCst);
    assert_eq!(dispatch!(), Ok(ExternalResult::Unknown));
    let op = s.operation(&h, "prepared").await.unwrap().unwrap();
    assert_eq!(op.request.recovery_until, Some(50));
    assert_eq!(
        op.request.recovery_proof.as_ref().unwrap().bytes,
        vec![1, 2, 3]
    );
    rt.model.fail.store(true, Ordering::SeqCst);
    assert_eq!(dispatch!(), Err(Error::UsageUnknown));
    assert_eq!(
        rt.model.prepared.load(Ordering::SeqCst),
        2,
        "existing unknown must not prepare again"
    );
    assert_eq!(
        s.account(&account).await.unwrap().reserved,
        Amount::new(10, 10)
    );
    assert_eq!(
        s.operation(&h, "prepared").await.unwrap().unwrap().request,
        op.request
    );
    *rt.model.generation.lock().unwrap() = "epoch-b".into();
    rt.model.found.store(true, Ordering::SeqCst);
    assert_eq!(
        dispatch!(),
        Err(Error::UsageUnknown),
        "changed adapter generation cannot validate original proof"
    );
    assert_eq!(
        s.account(&account).await.unwrap().reserved,
        Amount::new(10, 10)
    );
    *rt.model.generation.lock().unwrap() = "epoch-a".into();
    rt.model.found.store(true, Ordering::SeqCst);
    let failed = dispatch!().unwrap();
    s.record(&c, &op.id, failed.clone(), 0).await.unwrap();
    let saved = s.operation(&h, "prepared").await.unwrap().unwrap();
    assert_eq!(saved.result, Some(failed));
    assert_eq!(
        saved.usage_observations.len(),
        0,
        "logical observations are not physical sends"
    );
    assert_eq!(s.account(&account).await.unwrap().used, Amount::new(0, 2));
    assert_eq!(
        s.account(&account).await.unwrap().reserved,
        Amount::default()
    );
    // A new billed attempt may obtain a new proof, but inherits the old absolute deadline.
    rt.model.fail.store(false, Ordering::SeqCst);
    rt.model.expires.store(200, Ordering::SeqCst);
    let mut retry = saved.request.clone();
    retry.attempt = 2;
    let result = rt
        .dispatch(
            &c,
            retry,
            "model",
            |ctx| {
                let model = &rt.model;
                let request = request.clone();
                async move { model.invoke(&ctx, request).await }
            },
            |_| async { Ok(Lookup::Unknown) },
        )
        .await
        .unwrap();
    assert_eq!(result, ExternalResult::Unknown);
    let retry = s.operation(&h, "prepared").await.unwrap().unwrap();
    assert_eq!(retry.request.recovery_until, Some(50));
    assert_eq!(
        retry.request.recovery_proof.as_ref().unwrap().expires_at,
        200
    );
    let retry_failure = ExternalResult::Failed {
        physical_evidence: Some(evidence()),
        code: "retry failed".into(),
        usage: Usage::measured(0, 0, 0),
        retry_after: None,
    };
    s.record(&c, &retry.id, retry_failure, 0).await.unwrap();
    let second = s
        .begin(
            &c,
            DispatchRequest::new(
                "success-evidence",
                OperationKind::Tool,
                "local",
                json!({}),
                Amount::new(0, 3),
            ),
            0,
        )
        .await
        .unwrap();
    let success = ExternalResult::Applied {
        physical_evidence: Some(PhysicalSendEvidence {
            completeness: EvidenceCompleteness::Complete,
            ..evidence()
        }),
        output: json!({}),
        usage: Usage::measured(0, 0, 3),
        external_id: None,
    };
    s.record(&c, &second.id, success.clone(), 0).await.unwrap();
    s.record(&c, &second.id, success.clone(), 0).await.unwrap();
    assert_eq!(
        s.operation(&h, "success-evidence")
            .await
            .unwrap()
            .unwrap()
            .result,
        Some(success)
    );
    assert_eq!(s.account(&account).await.unwrap().used, Amount::new(0, 5));
    let mut expiring = DispatchRequest::new(
        "expired-before-send",
        OperationKind::Tool,
        "job",
        json!({}),
        Amount::new(0, 3),
    );
    expiring.recovery_proof = Some(RecoveryProof {
        bytes: vec![4],
        generation: "epoch-a".into(),
        expires_at: 10,
    });
    expiring.recovery_until = Some(10);
    let pending = s.begin(&c, expiring, 0).await.unwrap();
    assert_eq!(
        s.mark_sent(&c, &pending.id, 10).await,
        Err(Error::UsageUnknown)
    );
    assert_eq!(
        s.account(&account).await.unwrap().reserved,
        Amount::new(0, 3)
    );
    assert_eq!(
        s.operation(&h, "expired-before-send")
            .await
            .unwrap()
            .unwrap()
            .send_count,
        0
    );
    let oversized = PhysicalSendEvidence {
        observations: vec![PhysicalSendObservation {
            sequence: 0,
            bytes: vec![0; 65_537],
        }],
        completeness: EvidenceCompleteness::Complete,
    };
    let invalid = ExternalResult::Failed {
        physical_evidence: Some(oversized),
        code: "invalid".into(),
        usage: Usage::measured(0, 0, 0),
        retry_after: None,
    };
    assert!(s.observe_result(&c, &second.id, invalid, 0).await.is_err());
}

struct PreparedJob(Arc<PreparedModel>);
impl Tool for PreparedJob {
    fn descriptor(&self) -> ToolDescriptor {
        crate::BusinessTool::new("prepared-job").descriptor()
    }
    fn requires_recovery_preparation(&self) -> bool {
        true
    }
    async fn prepare_recovery(
        &self,
        c: &PreparationContext,
        input: &Value,
    ) -> Result<Option<RecoveryProof>> {
        self.0.prepare_recovery(c, input).await
    }
    async fn invoke(&self, c: &ToolContext, _: Value) -> Result<ExternalResult> {
        self.0
            .seen
            .lock()
            .unwrap()
            .push(c.recovery_proof.clone().unwrap());
        Ok(ExternalResult::Unknown)
    }
    async fn lookup(&self, c: &ToolContext, _: Value) -> Result<Lookup> {
        self.0
            .lookup(
                c,
                ModelRequest {
                    model: "fake".into(),
                    messages: vec![],
                    tools: vec![],
                    max_output: 1,
                    metadata: Value::Null,
                },
            )
            .await
    }
}
pub async fn job_preparation<S: ControlStore>(s: Arc<S>) {
    use runtrell::assembly::RegisteredTool;
    let account = s
        .create_account("t", Amount::new(100, 100), None)
        .await
        .unwrap();
    let h = s
        .start(RunSpec::new("t", "prepared-job", account.clone(), "job"))
        .await
        .unwrap();
    let c = s.claim(&h, "w", 0, 100).await.unwrap();
    let state = Arc::new(PreparedModel {
        fail: AtomicBool::new(true),
        prepared: AtomicUsize::new(0),
        expires: AtomicU64::new(50),
        found: AtomicBool::new(false),
        unsent: AtomicBool::new(false),
        seen: Mutex::new(vec![]),
        generation: Mutex::new("epoch-a".into()),
    });
    let tool = RegisteredTool::new(PreparedJob(state.clone()));
    let rt = Runtime {
        store: s.clone(),
        model: crate::FakeModel::scripted(vec![]),
        tools: vec![tool.clone()],
        policy: crate::FakePolicy::default(),
        clock: crate::TestClock::default(),
        validator: Arc::new(crate::DecisionValidator::default()),
        finalizer: Arc::new(crate::DecisionFinalizer),
    };
    let mut d = DispatchRequest::new(
        "job",
        OperationKind::Tool,
        "prepared-job",
        json!({}),
        Amount::new(0, 10),
    );
    d.scope = Some(runtrell::file_tools::catalog_digest(&tool.descriptor()));
    macro_rules! dispatch {
        () => {
            rt.dispatch(
                &c,
                d.clone(),
                "prepared-job",
                |ctx| {
                    let tool = &tool;
                    async move { tool.invoke(&ctx, json!({})).await }
                },
                |ctx| {
                    let tool = &tool;
                    async move { tool.lookup(&ctx, json!({})).await }
                },
            )
            .await
        };
    }
    assert_eq!(dispatch!(), Err(Error::Storage));
    assert!(s.list_operations(&h).await.unwrap().is_empty());
    assert_eq!(
        s.account(&account).await.unwrap().reserved,
        Amount::default()
    );
    state.fail.store(false, Ordering::SeqCst);
    assert_eq!(dispatch!(), Ok(ExternalResult::Unknown));
    let original = s.operation(&h, "job").await.unwrap().unwrap();
    state.fail.store(true, Ordering::SeqCst);
    assert_eq!(dispatch!(), Err(Error::UsageUnknown));
    assert_eq!(state.prepared.load(Ordering::SeqCst), 2);
    assert_eq!(
        s.operation(&h, "job").await.unwrap().unwrap().request,
        original.request
    );
    assert_eq!(
        s.account(&account).await.unwrap().reserved,
        Amount::new(0, 10)
    );
    state.unsent.store(true, Ordering::SeqCst);
    assert_eq!(dispatch!(), Ok(ExternalResult::Unknown));
    assert_eq!(state.prepared.load(Ordering::SeqCst), 2);
    let replayed = s.operation(&h, "job").await.unwrap().unwrap();
    assert_eq!(replayed.request, original.request);
    assert_eq!(replayed.send_count, 2);
    state.unsent.store(false, Ordering::SeqCst);
    state.found.store(true, Ordering::SeqCst);
    assert!(matches!(
        dispatch!().unwrap(),
        ExternalResult::Failed { .. }
    ));
    assert_eq!(s.account(&account).await.unwrap().used, Amount::new(0, 2));
    assert_eq!(state.seen.lock().unwrap().len(), 2);
}
