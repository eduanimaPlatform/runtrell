// SPDX-License-Identifier: Apache-2.0
use super::*;
use runtrell_testkit::{DecisionFinalizer, DecisionValidator, FakePolicy, TestClock};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) fn reply(parts: Value) -> Value {
    let mut result = response("gemini", "completed", false);
    result["usageMetadata"] =
        json!({"promptTokenCount":3,"candidatesTokenCount":1,"thoughtsTokenCount":0});
    result["candidates"][0]["content"]["parts"] = parts;
    result
}

#[tokio::test]
async fn final_submission_repairs_format_and_purges_the_confirmed_transcript() {
    let (endpoint, wire) = super::document_contracts::sequence_server(vec![
        reply(json!([{"text":"{\"private_draft\":true}"}])),
        reply(json!([{"text":"{\"decision\":\"answered\"}"}])),
    ])
    .await;
    let store = Arc::new(memory::MemoryStore::default());
    let account = store
        .create_account("tenant", Amount::new(10000, 10000), None)
        .await
        .unwrap();
    let mut spec = RunSpec::new("tenant", "request", account.clone(), "private input");
    spec.completion.output_schema = Some(json!({
        "type":"object","properties":{"decision":{"type":"string"}},
        "required":["decision"],"additionalProperties":false
    }));
    spec.completion.provider_format = true;
    spec.transcript = TranscriptPolicy::PurgeOnSuccess;
    let handle = store.start(spec.clone()).await.unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let claim = store.claim(&handle, "worker", now, 60_000).await.unwrap();
    let clock = TestClock::default();
    clock.set(now);
    let mut config = config(endpoint);
    config.run = handle.clone();
    config.capabilities.json_schema = true;
    config.capabilities.limits.price = Some(Price {
        model: "host-model".into(),
        input: 1,
        cached: 1,
        output: 1,
        reasoning: 1,
        image_input: 1,
        image_output: 1,
        cached_image_input: 1,
    });
    let runtime = Runtime {
        store: store.clone(),
        model: Gemini::new(config).unwrap(),
        tools: Vec::<runtrell_testkit::BusinessTool>::new(),
        policy: FakePolicy::default(),
        clock,
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    let waiting = runtime.drive(&claim).await.unwrap();
    assert_eq!(waiting.state, RunState::Waiting("validation repair".into()));
    assert!(
        waiting
            .checkpoint
            .unwrap()
            .messages
            .iter()
            .any(|m| m.text.contains("private_draft"))
    );
    let done = runtime.drive(&claim).await.unwrap();
    assert_eq!(done.state, RunState::Terminal(Terminal::Succeeded));
    assert!(done.transcript_purged());
    assert!(done.spec.input.is_empty());
    assert!(done.output.is_none());
    assert_eq!(store.start(spec).await.unwrap(), handle);
    let ops = store.list_operations(&handle).await.unwrap();
    assert_eq!(ops.len(), 2);
    assert!(
        ops.iter()
            .all(|o| o.settled && o.request.input.is_null() && o.finished_at.is_some())
    );
    let kept = serde_json::to_string(&ops).unwrap();
    assert!(
        !kept.contains("private_draft")
            && !kept.contains("private input")
            && !kept.contains("answered")
    );
    assert_eq!(store.account(&account).await.unwrap().used.tokens, 8);
    let wire = wire.await.unwrap();
    assert_eq!(wire.len(), 2);
    assert_eq!(
        wire[0]["generationConfig"]["responseJsonSchema"]["required"],
        json!(["decision"])
    );
    assert!(wire[1].to_string().contains("private_draft"));
}

fn fixture_config(endpoint: String, run: RunHandle) -> Config {
    let mut fixture = config(endpoint);
    fixture.run = run;
    fixture.capabilities.limits.price = Some(Price {
        model: "host-model".into(),
        input: 1,
        cached: 1,
        output: 1,
        reasoning: 1,
        image_input: 1,
        image_output: 1,
        cached_image_input: 1,
    });
    fixture
}

async fn setup(spec: RunSpec, store: &memory::MemoryStore) -> (RunHandle, Claim, TestClock) {
    let handle = store.start(spec).await.unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let claim = store.claim(&handle, "worker", now, 60_000).await.unwrap();
    let clock = TestClock::default();
    clock.set(now);
    (handle, claim, clock)
}

fn call(id: &str, name: &str, input: Value) -> Value {
    reply(json!([{"functionCall":{"id":id,"name":name,"args":input}}]))
}

struct ValidationFixture {
    basis: Arc<std::sync::atomic::AtomicU64>,
    repair: RepairPolicy,
    diagnostics: Vec<Diagnostic>,
}
impl Validator for ValidationFixture {
    fn repair_policy(&self) -> RepairPolicy {
        self.repair.clone()
    }
    fn completion_basis(&self, _: &RunRecord) -> Result<String> {
        Ok(self
            .basis
            .load(std::sync::atomic::Ordering::SeqCst)
            .to_string())
    }
    fn validate(&self, run: &RunRecord, candidate: &Value) -> Result<ValidationReceipt> {
        Ok(ValidationReceipt {
            hash: digest(candidate),
            basis_revision: run.spec.basis_revision,
            config_version: run.spec.config_version.clone(),
            validator: run.spec.validator.clone(),
            validator_version: run.spec.validator_version.clone(),
            passed: self.diagnostics.is_empty(),
            repairable: !self.diagnostics.is_empty(),
            diagnostics: self.diagnostics.clone(),
        })
    }
}
struct ValidateTool {
    store: Arc<memory::MemoryStore>,
    validator: Arc<ValidationFixture>,
}
impl Tool for ValidateTool {
    fn descriptor(&self) -> ToolDescriptor {
        runtrell_testkit::BusinessTool::new("validate").descriptor()
    }
    async fn invoke(&self, context: &ToolContext, candidate: Value) -> Result<ExternalResult> {
        let run = self.store.get(&context.claim.run).await?;
        let output = ValidationFeedback {
            receipt: self.validator.validate(&run, &candidate)?,
            basis: self.validator.completion_basis(&run)?,
            candidate,
            repair_target: RepairTarget::Workspace,
        };
        Ok(ExternalResult::Applied {
            output: json!(output),
            usage: Usage::measured(0, 0, 0),
            external_id: None,
            physical_evidence: None,
        })
    }
    async fn lookup(&self, _: &ToolContext, _: Value) -> Result<Lookup> {
        Ok(Lookup::Unknown)
    }
}

#[tokio::test]
async fn validated_ack_keeps_the_candidate_and_uses_a_tool_free_wire_request() {
    let candidate = json!({"decision":"fixture result"});
    let (endpoint, wire) = super::document_contracts::sequence_server(vec![
        call("check", "validate", candidate.clone()),
        reply(json!([{"text":"{\"acknowledged\":true}"}])),
    ])
    .await;
    let store = Arc::new(memory::MemoryStore::default());
    let account = store
        .create_account("tenant", Amount::new(10000, 10000), None)
        .await
        .unwrap();
    let mut spec = RunSpec::new("tenant", "ack", account, "long private context");
    spec.completion.mode = CompletionMode::ValidatedAck;
    spec.completion.validation_tool = Some("validate".into());
    spec.completion.provider_format = true;
    let (handle, claim, clock) = setup(spec, &store).await;
    let validator = Arc::new(ValidationFixture {
        basis: Arc::default(),
        repair: RepairPolicy::default(),
        diagnostics: vec![],
    });
    let done = Runtime {
        store: store.clone(),
        model: Gemini::new(fixture_config(endpoint, handle)).unwrap(),
        tools: vec![ValidateTool {
            store,
            validator: validator.clone(),
        }],
        policy: FakePolicy::default(),
        clock,
        validator,
        finalizer: Arc::new(DecisionFinalizer),
    }
    .drive(&claim)
    .await
    .unwrap();
    assert_eq!(done.state, RunState::Terminal(Terminal::Succeeded));
    assert_eq!(done.output, Some(candidate));
    let sent = wire.await.unwrap();
    assert_eq!(sent.len(), 2);
    assert!(sent[0].to_string().contains("long private context"));
    assert!(sent[1].get("tools").is_none());
    let short = sent[1].to_string();
    assert!(!short.contains("long private context") && !short.contains("fixture result"));
    assert_eq!(
        sent[1]["generationConfig"]["responseJsonSchema"]["required"],
        json!(["acknowledged"])
    );
}

#[tokio::test]
async fn tool_and_final_validation_share_observed_repair_progress() {
    let (endpoint, wire) = super::document_contracts::sequence_server(vec![
        reply(json!([{"text":"{\"decision\":\"draft\"}"}])),
        call(
            "check",
            "validate",
            json!({"decision":"different envelope"}),
        ),
    ])
    .await;
    let store = Arc::new(memory::MemoryStore::default());
    let account = store
        .create_account("tenant", Amount::new(10000, 10000), None)
        .await
        .unwrap();
    let mut spec = RunSpec::new("tenant", "repair", account, "prepare a submission");
    spec.completion.validation_tool = Some("validate".into());
    let (handle, claim, clock) = setup(spec, &store).await;
    let validator = Arc::new(ValidationFixture {
        basis: Arc::default(),
        repair: RepairPolicy {
            max_no_progress: std::num::NonZeroU64::new(2),
            ..RepairPolicy::default()
        },
        diagnostics: vec![Diagnostic {
            repair: DiagnosticRepair::Agent,
            observation: Some(ArtifactObservation {
                artifact: "document".into(),
                version: ArtifactVersion::ContentDigest("fixed".into()),
            }),
            code: "MISSING_SECTION".into(),
            severity: "error".into(),
            path: "/section".into(),
            schema_path: None,
            artifact: None,
            file: None,
            field: None,
            span: None,
            position: None,
            cause: "missing required section".into(),
            hint: "edit the current document".into(),
        }],
    });
    let runtime = Runtime {
        store: store.clone(),
        model: Gemini::new(fixture_config(endpoint, handle)).unwrap(),
        tools: vec![ValidateTool {
            store,
            validator: validator.clone(),
        }],
        policy: FakePolicy::default(),
        clock,
        validator,
        finalizer: Arc::new(DecisionFinalizer),
    };
    assert_eq!(
        runtime.drive(&claim).await.unwrap().state,
        RunState::Waiting("validation repair".into())
    );
    let stopped = runtime.drive(&claim).await.unwrap();
    assert!(matches!(
        stopped.state,
        RunState::Terminal(Terminal::Stalled(ProgressStop::Validation { .. }))
    ));
    assert_eq!(
        stopped.checkpoint.unwrap().payload["repair_progress"]["occurrences"],
        2
    );
    let sent = wire.await.unwrap();
    assert_eq!(sent.len(), 2);
    assert!(sent[1].get("tools").is_some());
    assert!(sent[1].to_string().contains("MISSING_SECTION"));
}

#[tokio::test]
async fn compaction_preserves_new_read_ranges_and_stops_before_another_stalled_summary() {
    use runtrell::{
        data::{Access, DataStore, StoragePolicy},
        file_tools::{FileAction, FileTool},
    };
    let store = Arc::new(memory::MemoryStore::default());
    let account = store
        .create_account("tenant", Amount::new(10000, 10000), None)
        .await
        .unwrap();
    let mut spec = RunSpec::new("tenant", "compact", account, "read the file");
    spec.compaction = Some(CompactionPolicy {
        min_remaining_tokens: 9000,
        preserve_progress: true,
        max_without_progress: std::num::NonZeroU64::new(1),
    });
    let (handle, claim, clock) = setup(spec, &store).await;
    let data = DataStore::memory(StoragePolicy::default());
    let access = Access::owner(Namespace {
        owner: handle.clone(),
        agent: "worker".into(),
    });
    let file = data
        .write(&access, "input.txt", None, b"some input content")
        .await
        .unwrap();
    let args = json!({"path":file.path,"revision":file.revision,"offset":0,"length":4});
    let (endpoint, wire) = super::document_contracts::sequence_server(vec![
        reply(json!([{"text":"initial summary"}])),
        reply(json!([
            {"functionCall":{"id":"read1","name":"files.read","args":args.clone()}},
            {"functionCall":{"id":"save","name":"files.write","args":{
                "path":"output.txt","expected_revision":null,"text":"file transcript body"
            }}},
        ])),
        reply(json!([{"text":"new progress summary"}])),
        call("read2", "files.read", args),
    ])
    .await;
    let stopped = Runtime {
        store: store.clone(),
        model: Gemini::new(fixture_config(endpoint, handle.clone())).unwrap(),
        tools: vec![
            FileTool {
                store: data.clone(),
                access: access.clone(),
                action: FileAction::Read,
            },
            FileTool {
                store: data,
                access,
                action: FileAction::Write,
            },
        ],
        policy: FakePolicy::default(),
        clock,
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    }
    .drive(&claim)
    .await
    .unwrap();
    assert!(matches!(
        stopped.state,
        RunState::Terminal(Terminal::Stalled(ProgressStop::Compaction))
    ));
    let sent = wire.await.unwrap();
    assert_eq!(sent.len(), 4);
    let summary_request = sent[2].to_string();
    assert!(
        summary_request.contains("output.txt") && !summary_request.contains("file transcript body")
    );
    assert!(
        stopped.checkpoint.unwrap().payload["context"]["facts"]
            .to_string()
            .contains("file transcript body")
    );
    let wire_progress = sent[3].to_string();
    assert!(
        wire_progress.contains("Recorded tool progress") && wire_progress.contains("input.txt")
    );
    assert_eq!(
        store
            .list_operations(&handle)
            .await
            .unwrap()
            .iter()
            .filter(|op| op.request.kind == OperationKind::Compaction)
            .count(),
        2
    );
}
