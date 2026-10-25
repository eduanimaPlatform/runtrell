// SPDX-License-Identifier: Apache-2.0
use runtrell::{
    assembly::RegisteredTool,
    data::*,
    file_tools::{FileAction, FileTool},
    memory::MemoryStore,
    *,
};
use runtrell_testkit::*;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
#[derive(Clone)]
struct DelayedStore {
    inner: DataStore,
    gate: Arc<tokio::sync::Barrier>,
    entered: Arc<AtomicUsize>,
}

impl WorkspaceStore for DelayedStore {
    async fn snapshot(&self, a: &Access) -> Result<WorkspaceSnapshot> {
        self.inner.snapshot(a).await
    }
    async fn list(
        &self,
        a: &Access,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<Page<FileVersion>> {
        self.inner.list(a, cursor, limit).await
    }
    async fn read(
        &self,
        a: &Access,
        path: &str,
        revision: u64,
        offset: u64,
        length: usize,
    ) -> Result<ByteRange> {
        self.inner.read(a, path, revision, offset, length).await
    }
    async fn write(
        &self,
        a: &Access,
        path: &str,
        expected: Option<u64>,
        bytes: &[u8],
    ) -> Result<FileVersion> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        self.gate.wait().await;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        self.inner.write(a, path, expected, bytes).await
    }
    async fn delete(&self, a: &Access, path: &str, expected: u64) -> Result<()> {
        self.inner.delete(a, path, expected).await
    }
    async fn search(
        &self,
        a: &Access,
        query: &str,
        cursor: Option<String>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<Page<Match>> {
        self.inner.search(a, query, cursor, limit, max_bytes).await
    }
    async fn merge(
        &self,
        a: &Access,
        base: &WorkspaceSnapshot,
        changes: Vec<Change>,
    ) -> Result<WorkspaceSnapshot> {
        self.inner.merge(a, base, changes).await
    }
}

impl ArtifactStore for DelayedStore {
    async fn put(
        &self,
        a: &Access,
        bytes: &[u8],
        class: DataClass,
        now: u64,
    ) -> Result<ArtifactRef> {
        self.inner.put(a, bytes, class, now).await
    }
    async fn read_artifact(
        &self,
        a: &Access,
        artifact: &ArtifactRef,
        offset: u64,
        length: usize,
    ) -> Result<ByteRange> {
        self.inner.read_artifact(a, artifact, offset, length).await
    }
    async fn delete_artifact(
        &self,
        a: &Access,
        artifact: &ArtifactRef,
        now: u64,
    ) -> Result<Progress> {
        self.inner.delete_artifact(a, artifact, now).await
    }
}

impl StandardToolStore for DelayedStore {
    fn policy(&self) -> &StoragePolicy {
        self.inner.policy()
    }
    async fn exact_replace(
        &self,
        a: &Access,
        expected: u64,
        edits: Vec<ExactEdit>,
    ) -> Result<WorkspaceSnapshot> {
        self.inner.exact_replace(a, expected, edits).await
    }
    async fn store_output(
        &self,
        a: &Access,
        operation: &str,
        value: Value,
        inline_limit: usize,
        now: u64,
    ) -> Result<StoredOutput> {
        self.inner
            .store_output(a, operation, value, inline_limit, now)
            .await
    }
}
struct Compute;
impl Tool for Compute {
    fn descriptor(&self) -> ToolDescriptor {
        BusinessTool::new("compute").descriptor()
    }
    async fn invoke(&self, _: &ToolContext, _: Value) -> Result<ExternalResult> {
        Ok(ExternalResult::Applied {
            physical_evidence: None,
            output: json!({"sum":2+2}),
            usage: Usage::measured(0, 0, 0),
            external_id: None,
        })
    }
    async fn lookup(&self, c: &ToolContext, v: Value) -> Result<Lookup> {
        Ok(Lookup::Found(self.invoke(c, v).await?))
    }
}
struct Job;
impl Tool for Job {
    fn descriptor(&self) -> ToolDescriptor {
        BusinessTool::new("job").descriptor()
    }
    async fn invoke(&self, _: &ToolContext, _: Value) -> Result<ExternalResult> {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        Ok(ExternalResult::Applied {
            physical_evidence: None,
            output: json!({"job":"done"}),
            usage: Usage::measured(0, 0, 0),
            external_id: None,
        })
    }
    async fn lookup(&self, c: &ToolContext, v: Value) -> Result<Lookup> {
        Ok(Lookup::Found(self.invoke(c, v).await?))
    }
}
struct SlowValidator(Arc<AtomicUsize>);
impl Validator for SlowValidator {
    fn validate(&self, r: &RunRecord, v: &Value) -> Result<ValidationReceipt> {
        self.0.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(50));
        self.0.fetch_sub(1, Ordering::SeqCst);
        DecisionValidator::default().validate(r, v)
    }
}
struct SlowFinalizer(Arc<AtomicUsize>);
impl Finalizer for SlowFinalizer {
    fn finalize(&self, r: &RunRecord, v: Value, receipt: ValidationReceipt) -> Result<Completion> {
        self.0.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(50));
        self.0.fetch_sub(1, Ordering::SeqCst);
        DecisionFinalizer.finalize(r, v, receipt)
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn heterogeneous_runs_yield_during_async_storage_and_sync_evidence_io() {
    let store = Arc::new(MemoryStore::default());
    let gate = Arc::new(tokio::sync::Barrier::new(6));
    let entered = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let heartbeat = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let ticker = tokio::spawn({
        let active = active.clone();
        let heartbeat = heartbeat.clone();
        let stop = stop.clone();
        async move {
            while !stop.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                if active.load(Ordering::SeqCst) > 0 {
                    heartbeat.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
    });
    let mut jobs = tokio::task::JoinSet::new();
    for index in 0..6 {
        let a = store
            .create_account("t", Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let h = store
            .start(RunSpec::new("t", &format!("parallel-{index}"), a, "judge"))
            .await
            .unwrap();
        let c = store.claim(&h, "w", 0, 100).await.unwrap();
        let data = DelayedStore {
            inner: DataStore::memory(StoragePolicy::default()),
            gate: gate.clone(),
            entered: entered.clone(),
        };
        let access = Access::owner(Namespace {
            owner: h,
            agent: "main".into(),
        });
        let rt = Runtime {
            store: store.clone(),
            model: FakeModel::scripted(vec![
                ModelReply {
                    text: String::new(),
                    calls: vec![
                        ToolCall {
                            id: "c".into(),
                            name: "compute".into(),
                            arguments: json!({}),
                        },
                        ToolCall {
                            id: "j".into(),
                            name: "job".into(),
                            arguments: json!({}),
                        },
                        ToolCall {
                            id: "f".into(),
                            name: "files.write".into(),
                            arguments: json!({"path":"answer.txt","expected_revision":null,"text":"done"}),
                        },
                    ],
                    complete: true,
                    metadata: Value::Null,
                },
                FakeModel::decision(json!({"decision":"done"})),
            ]),
            tools: vec![
                RegisteredTool::new(Compute),
                RegisteredTool::new(Job),
                RegisteredTool::new(FileTool {
                    store: data.clone(),
                    access: access.clone(),
                    action: FileAction::Write,
                }),
            ],
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: Arc::new(SlowValidator(active.clone())),
            finalizer: Arc::new(SlowFinalizer(active.clone())),
        };
        // tokio::spawn requires the entire standard Runtime future to be Send + 'static.
        jobs.spawn(async move {
            let done = rt.drive(&c).await.unwrap();
            assert_eq!(done.state, RunState::Terminal(Terminal::Succeeded));
            assert_eq!(data.snapshot(&access).await.unwrap().files.len(), 1);
        });
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(result) = jobs.join_next().await {
            result.unwrap();
        }
    })
    .await
    .expect("all Runs progress with a single Tokio worker");
    stop.store(true, Ordering::SeqCst);
    ticker.await.unwrap();
    assert_eq!(entered.load(Ordering::SeqCst), 6);
    assert!(
        heartbeat.load(Ordering::SeqCst) > 5,
        "synchronous evidence I/O must leave the Tokio worker available"
    );
}
