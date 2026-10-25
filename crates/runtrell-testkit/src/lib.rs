// SPDX-License-Identifier: Apache-2.0
//! Credential-free host adapters; no private repositories or network access.
use runtrell::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
#[derive(Clone, Default)]
pub struct TestClock(pub Arc<AtomicU64>);
impl Clock for TestClock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
    async fn wait_until(&self, deadline: u64) {
        std::future::poll_fn(|cx| {
            if self.now() >= deadline {
                std::task::Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        })
        .await
    }
}
impl TestClock {
    pub fn set(&self, time: u64) {
        self.0.store(time, Ordering::SeqCst);
    }
}
#[derive(Clone, Default)]
pub struct FakePolicy {
    pub revoked: Arc<AtomicBool>,
    pub allowed_tenant: Option<String>,
}
impl Policy for FakePolicy {
    fn authorize(&self, r: &RunRecord, _: &str, _: &str) -> Result<()> {
        if self.revoked.load(Ordering::SeqCst)
            || self
                .allowed_tenant
                .as_ref()
                .is_some_and(|t| t != &r.spec.tenant)
        {
            Err(Error::PermissionDenied)
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Default)]
pub struct FakeModel {
    pub replies: Arc<Mutex<VecDeque<ModelReply>>>,
    pub requests: Arc<Mutex<Vec<ModelRequest>>>,
    pub results: Arc<Mutex<BTreeMap<String, ExternalResult>>>,
    pub lose_response: Arc<AtomicBool>,
}
impl FakeModel {
    pub fn scripted(replies: Vec<ModelReply>) -> Self {
        Self {
            replies: Arc::new(Mutex::new(replies.into())),
            ..Self::default()
        }
    }
    pub fn decision(value: Value) -> ModelReply {
        ModelReply {
            text: value.to_string(),
            calls: vec![],
            complete: true,
            metadata: Value::Null,
        }
    }
    pub fn calls(calls: Vec<ToolCall>) -> ModelReply {
        ModelReply {
            text: String::new(),
            calls,
            complete: true,
            metadata: Value::Null,
        }
    }
}
impl ModelPort for FakeModel {
    fn limits(&self) -> ModelLimits {
        ModelLimits {
            model: "fake".into(),
            context: 1024,
            max_output: 16,
            min_max_output_tokens: 1,
            timeout: 30,
            price: Some(Price {
                model: "fake".into(),
                input: 1_000_000,
                cached: 0,
                output: 1_000_000,
                reasoning: 1_000_000,
                image_input: 1_000_000,
                cached_image_input: 0,
                image_output: 1_000_000,
            }),
        }
    }
    fn input_upper_bound(&self, r: &ModelRequest) -> Result<u64> {
        Ok(4 + r.messages.len() as u64)
    }
    fn recovery(&self) -> Option<(String, u64)> {
        Some(("fake:host".into(), 1000))
    }
    async fn invoke(&self, ctx: &ToolContext, r: ModelRequest) -> Result<ExternalResult> {
        self.requests.lock().unwrap().push(r);
        if let Some(old) = self.results.lock().unwrap().get(&ctx.operation).cloned() {
            return Ok(old);
        }
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| Error::Invalid("no fake reply".into()))?;
        let mut usage = Usage::measured(3, 1, 4);
        usage.model = "fake".into();
        usage.cached = Some(0);
        let result = ExternalResult::Applied {
            physical_evidence: None,
            output: serde_json::to_value(reply).unwrap(),
            usage,
            external_id: None,
        };
        self.results
            .lock()
            .unwrap()
            .insert(ctx.operation.clone(), result.clone());
        if self.lose_response.swap(false, Ordering::SeqCst) {
            Ok(ExternalResult::Unknown)
        } else {
            Ok(result)
        }
    }
    async fn lookup(&self, ctx: &ToolContext, _: ModelRequest) -> Result<Lookup> {
        Ok(self
            .results
            .lock()
            .unwrap()
            .get(&ctx.operation)
            .cloned()
            .map(Lookup::Found)
            .unwrap_or(Lookup::NotFound {
                scope: "fake:host".into(),
            }))
    }
}
#[derive(Clone)]
pub struct BusinessTool {
    pub name: String,
    pub effects: Arc<Mutex<BTreeMap<String, (String, Value)>>>,
    pub calls: Arc<AtomicU64>,
    pub lose_response: Arc<AtomicBool>,
    pub accepted: Arc<AtomicBool>,
}
impl BusinessTool {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            effects: Arc::default(),
            calls: Arc::default(),
            lose_response: Arc::default(),
            accepted: Arc::default(),
        }
    }
}
impl Tool for BusinessTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: self.name.clone(),
            version: "1".into(),
            account: "business:host".into(),
            permission: self.name.clone(),
            capability: "native-trusted".into(),
            input_schema: json!({"type":"object"}),
            output_schema: json!({"type":"object"}),
            side_effect: true,
            timeout: 10,
            max_output_bytes: 4096,
            reserve: Amount::new(0, 0),
            quantities: Default::default(),
            conflict_scope: None,
            retry: RetryPolicy {
                max_attempts: 3,
                backoff: 1,
                max_elapsed: 10,
            },
        }
    }
    async fn invoke(&self, _: &ToolContext, input: Value) -> Result<ExternalResult> {
        let key = input
            .get("effect_key")
            .and_then(Value::as_str)
            .unwrap_or("read")
            .to_string();
        let hash = digest(&input);
        let mut effects = self.effects.lock().unwrap();
        let output = if let Some((old, output)) = effects.get(&key) {
            if old != &hash {
                return Err(Error::Conflict);
            }
            output.clone()
        } else {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let output = json!({"answer":"available","effect_key":key});
            effects.insert(key.clone(), (hash, output.clone()));
            output
        };
        if self.lose_response.swap(false, Ordering::SeqCst) {
            return Ok(ExternalResult::Unknown);
        }
        if self.accepted.load(Ordering::SeqCst) {
            return Ok(ExternalResult::Accepted { external_id: key });
        }
        Ok(ExternalResult::Applied {
            physical_evidence: None,
            output,
            usage: Usage::measured(0, 0, 0),
            external_id: Some(key),
        })
    }
    async fn lookup(&self, _: &ToolContext, input: Value) -> Result<Lookup> {
        let key = input
            .get("effect_key")
            .and_then(Value::as_str)
            .unwrap_or("read");
        if self.accepted.load(Ordering::SeqCst) {
            return Ok(Lookup::Pending);
        }
        Ok(match self.effects.lock().unwrap().get(key) {
            Some((hash, output)) if hash == &digest(&input) => {
                Lookup::Found(ExternalResult::Applied {
                    physical_evidence: None,
                    output: output.clone(),
                    usage: Usage::measured(0, 0, 0),
                    external_id: Some(key.into()),
                })
            }
            Some(_) => return Err(Error::Conflict),
            None => Lookup::Unknown,
        })
    }
}
#[derive(Clone)]
pub struct DecisionValidator {
    pub schema: Value,
    pub version: String,
}
impl Default for DecisionValidator {
    fn default() -> Self {
        Self {
            schema: json!({"type":"object","properties":{"decision":{"type":"string"}},"required":["decision"],"additionalProperties":false}),
            version: "1".into(),
        }
    }
}
impl Validator for DecisionValidator {
    fn validate(&self, r: &RunRecord, v: &Value) -> Result<ValidationReceipt> {
        runtrell::validate_schema(v, &self.schema)?;
        if v["decision"] == "insufficient" {
            return Err(Error::Invalid("INSUFFICIENT_EVIDENCE".into()));
        }
        Ok(ValidationReceipt {
            hash: digest(v),
            basis_revision: r.spec.basis_revision,
            config_version: r.spec.config_version.clone(),
            validator: "host".into(),
            validator_version: self.version.clone(),
            passed: true,
            diagnostics: vec![],
        })
    }
}
#[derive(Clone, Default)]
pub struct DecisionFinalizer;
impl Finalizer for DecisionFinalizer {
    fn finalize(
        &self,
        r: &RunRecord,
        candidate: Value,
        receipt: ValidationReceipt,
    ) -> Result<Completion> {
        if !receipt.passed {
            return Err(Error::Invalid("validation failed".into()));
        }
        Ok(Completion {
            id: format!("completion:{}", r.handle.id),
            output: candidate,
            receipt,
        })
    }
}
/// Small executor for immediately-ready fake adapters. Real hosts supply an executor.
pub fn block_on<F: Future>(f: F) -> F::Output {
    static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    let rt = RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    });
    let _enter = if tokio::runtime::Handle::try_current().is_err() {
        Some(rt.enter())
    } else {
        None
    };
    let w = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(w);
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}
pub type FakeRuntime = Runtime<
    runtrell::memory::MemoryStore,
    FakeModel,
    BusinessTool,
    FakePolicy,
    TestClock,
    DecisionValidator,
    DecisionFinalizer,
>;
pub fn runtime(
    store: Arc<runtrell::memory::MemoryStore>,
    model: FakeModel,
    tools: Vec<BusinessTool>,
) -> FakeRuntime {
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
pub mod execution;
pub mod execution_contracts;
pub use execution::{FakeExecutionBackend, FakeExecutionRegistry};
pub mod budget_contracts;
pub mod recovery_contracts;

pub mod repair_contracts;
