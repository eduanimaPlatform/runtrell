// SPDX-License-Identifier: Apache-2.0
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use runtrell::{attachments::*, data::*, document_view::*, images::*, standard_tools::*};
use runtrell_postgres::PgStore;
use runtrell_testkit::{DecisionFinalizer, DecisionValidator, FakePolicy, TestClock};
use std::sync::Arc;
fn uuid_id() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

fn pages_pdf() -> Vec<u8> {
    use lopdf::{Document, Object, Stream, dictionary};
    let mut doc = Document::with_version("1.7");
    let tree = doc.new_object_id();
    let mut kids = Vec::new();
    for page in 1..=10 {
        let content = doc.add_object(Stream::new(
            dictionary! {},
            format!("BT (PAGE_{page:02}) Tj ET").into_bytes(),
        ));
        kids.push(Object::Reference(doc.add_object(dictionary! {"Type"=>"Page","Parent"=>tree,"Contents"=>content,"MediaBox"=>vec![0.into(),0.into(),100.into(),100.into()]})));
    }
    doc.objects.insert(
        tree,
        dictionary! {"Type"=>"Pages","Kids"=>kids,"Count"=>10}.into(),
    );
    let catalog = doc.add_object(dictionary! {"Type"=>"Catalog","Pages"=>tree});
    doc.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}
fn reply(parts: Value) -> Value {
    let mut response = response("gemini", "completed", false);
    response["usageMetadata"] =
        json!({"promptTokenCount":3,"candidatesTokenCount":1,"thoughtsTokenCount":0});
    response["candidates"][0]["content"]["parts"] = parts;
    response
}
fn call(id: &str, name: &str, args: Value) -> Value {
    reply(json!([{"functionCall":{"id":id,"name":name,"args":args}}]))
}
fn done() -> Value {
    reply(json!([{"text":"{\"decision\":\"answered\"}"}]))
}
fn truncated() -> Value {
    let mut response = reply(json!([{"text":"partial transcription"}]));
    response["candidates"][0]["finishReason"] = json!("MAX_TOKENS");
    response
}

// A normal tool returning several independently saved views in one result.
// This exercises count and bytes admission independently of page selection.
struct OpenViews {
    views: Vec<Attachment>,
}
impl Tool for OpenViews {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: "fixture.open".into(),
            version: "1".into(),
            account: "files".into(),
            permission: "read".into(),
            capability: "views".into(),
            input_schema: json!({"type":"object","properties":{"count":{"type":"integer","minimum":1,"maximum":5}},"required":["count"],"additionalProperties":false}),
            output_schema: json!({"type":"object"}),
            side_effect: false,
            timeout: 60,
            max_output_bytes: 16384,
            reserve: Amount::default(),
            quantities: Default::default(),
            conflict_scope: None,
            retry: RetryPolicy {
                max_attempts: 1,
                backoff: 0,
                max_elapsed: 60,
            },
        }
    }
    async fn invoke(&self, _: &ToolContext, input: Value) -> Result<ExternalResult> {
        Ok(ExternalResult::Applied {
            output: json!({"attachments":self.views[..input["count"].as_u64().unwrap() as usize]}),
            physical_evidence: None,
            usage: Usage::measured(0, 0, 0),
            external_id: None,
        })
    }
    async fn lookup(&self, _: &ToolContext, _: Value) -> Result<Lookup> {
        Ok(Lookup::Unknown)
    }
}

#[tokio::test]
async fn tool_count_and_byte_overflow_are_repairable_before_inference_and_preserve_receipts() {
    for by_bytes in [false, true] {
        let store = Arc::new(memory::MemoryStore::default());
        let account = store
            .create_account("tenant", Amount::new(10000, 10000), None)
            .await
            .unwrap();
        let handle = store
            .start(RunSpec::new(
                "tenant",
                "limits",
                account,
                "open fewer views when needed",
            ))
            .await
            .unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let claim = store.claim(&handle, "worker", now, 100).await.unwrap();
        let access = Access::owner(Namespace {
            owner: handle.clone(),
            agent: "main".into(),
        });
        let data = DataStore::memory(StoragePolicy::default());
        let bytes = select_pdf(
            &pages_pdf(),
            Some(&PageRange { first: 1, last: 2 }),
            4,
            100000,
        )
        .unwrap()
        .1
        .unwrap();
        let mut views = Vec::new();
        for i in 0..5 {
            let staged = data
                .stage_named(
                    &access,
                    ArtifactSpec {
                        id: format!("view-{i}"),
                        media_type: "application/pdf".into(),
                        class: DataClass::Intermediate,
                    },
                    &bytes,
                    now,
                )
                .unwrap();
            views.push(Attachment::Stored(ImageReference::Artifact(
                data.save(&access, staged, now).await.unwrap(),
            )));
        }
        let (endpoint, http) = super::document_contracts::sequence_server(vec![
            call("too-many", "fixture.open", json!({"count":5})),
            call("narrow", "fixture.open", json!({"count":1})),
            done(),
        ])
        .await;
        let mut config = model_config(endpoint, handle.clone());
        if by_bytes {
            config.document_limits.max_inputs = 5;
            config.document_limits.max_total_bytes = bytes.len() as u64 * 2;
        }
        let clock = TestClock::default();
        clock.set(now);
        let runtime = Runtime {
            store: store.clone(),
            model: ImageModel {
                inner: Gemini::new(config).unwrap(),
                resolver: ImageResolver {
                    store: data,
                    policy: FakePolicy::default(),
                    accesses: vec![access],
                    limits: ImageLimits::default(),
                },
                runs: store.clone(),
            },
            tools: vec![OpenViews { views }],
            policy: FakePolicy::default(),
            clock,
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let run = runtime.drive(&claim).await.unwrap();
        assert_eq!(run.state, RunState::Terminal(Terminal::Succeeded));
        let requests = http.await.unwrap();
        assert_eq!(requests.len(), 3);
        let wire = requests[1].to_string();
        assert!(wire.contains("MODEL_ATTACHMENT_LIMIT") && wire.contains("narrower page range"));
        assert!(!wire.contains("inlineData"));
        let last = requests[2]["contents"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|c| c["parts"].as_array().unwrap())
            .filter_map(|p| p["inlineData"]["data"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(last.len(), 1);
        assert_eq!(STANDARD.decode(last[0]).unwrap(), bytes);
        let ops = store.list_operations(&handle).await.unwrap();
        let broad = ops
            .iter()
            .find(|o| o.request.target == "fixture.open" && o.request.input["count"] == 5)
            .unwrap();
        let Some(ExternalResult::Applied { output, .. }) = &broad.result else {
            panic!()
        };
        assert_eq!(output["attachments"].as_array().unwrap().len(), 5);
        let cp = run.checkpoint.unwrap();
        let feedback = cp
            .messages
            .iter()
            .find(|m| m.data["code"] == "MODEL_ATTACHMENT_LIMIT")
            .unwrap();
        assert_eq!(
            feedback.data["result"]["attachments"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
    }
}
fn model_config(endpoint: String, handle: RunHandle) -> Config {
    let mut c = config(endpoint);
    c.run = handle;
    c.capabilities.tool_calling = true;
    c.capabilities.documents.pdf.inline_bytes = true;
    c.capabilities.limits.timeout = 60;
    c.capabilities.limits.price = Some(Price {
        model: "host-model".into(),
        input: 1,
        cached: 1,
        output: 1,
        reasoning: 1,
        image_input: 1,
        image_output: 1,
        cached_image_input: 1,
    });
    c
}

#[tokio::test]
async fn image_window_survives_two_continuations_and_closes_after_tool_call() {
    let store = Arc::new(memory::MemoryStore::default());
    let account = store
        .create_account("tenant", Amount::new(100000, 100000), None)
        .await
        .unwrap();
    let handle = store
        .start(RunSpec::new(
            "tenant",
            "image-continuation",
            account,
            "transcribe image",
        ))
        .await
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claim = store.claim(&handle, "worker", now, 100).await.unwrap();
    let access = Access::owner(Namespace {
        owner: handle.clone(),
        agent: "main".into(),
    });
    let data = DataStore::memory(StoragePolicy::default());
    let bytes = runtrell_testkit::image_contracts::png();
    let file = data
        .write(&access, "input.png", None, &bytes)
        .await
        .unwrap();
    let attachment = Attachment::Stored(ImageReference::Workspace {
        namespace: access.namespace().clone(),
        file,
        mime_type: "image/png".into(),
    });
    let (endpoint, http) = super::document_contracts::sequence_server(vec![
        call("view", "document.view", json!({"attachment":attachment})),
        truncated(),
        truncated(),
        call(
            "save",
            "files.write",
            json!({"path":"ocr.md","expected_revision":null,"text":"transcribed"}),
        ),
        done(),
    ])
    .await;
    let mut config = model_config(endpoint, handle.clone());
    config.capabilities.images.input = true;
    let clock = TestClock::default();
    clock.set(now);
    let resolver = || ImageResolver {
        store: data.clone(),
        policy: FakePolicy::default(),
        accesses: vec![access.clone()],
        limits: ImageLimits::default(),
    };
    let runtime = Runtime {
        store: store.clone(),
        model: ImageModel {
            inner: Gemini::new(config).unwrap(),
            resolver: resolver(),
            runs: store.clone(),
        },
        tools: ToolSet::default()
            .files(data.clone(), access.clone())
            .select(StandardTool::Write)
            .document_view(DocumentView {
                resolver: resolver(),
                external: None,
                runs: store.clone(),
                clock: clock.clone(),
                output_access: access,
                limits: DocumentViewLimits::default(),
            })
            .select(StandardTool::DocumentView)
            .build()
            .unwrap(),
        policy: FakePolicy::default(),
        clock,
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    assert_eq!(
        runtime.drive(&claim).await.unwrap().state,
        RunState::Terminal(Terminal::Succeeded)
    );
    let captures = http.await.unwrap();
    assert_eq!(captures.len(), 5);
    for (index, request) in captures.iter().enumerate() {
        let payloads = request["contents"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|c| c["parts"].as_array().unwrap())
            .filter_map(|p| p["inlineData"]["data"].as_str())
            .map(|s| STANDARD.decode(s).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            payloads,
            if (1..=3).contains(&index) {
                vec![bytes.clone()]
            } else {
                vec![]
            },
            "request {index}"
        );
    }
    // The selected fixed references and their history remain persisted.
    let ops = store.list_operations(&handle).await.unwrap();
    let models = ops
        .iter()
        .filter(|o| o.request.kind == OperationKind::Model)
        .collect::<Vec<_>>();
    assert_eq!(
        models
            .iter()
            .filter(|o| !o.request.input["images"].as_array().unwrap().is_empty())
            .count(),
        3
    );
}

// Stop only before the next model operation is recorded, after the preceding
// third view has been checkpointed, with its attachments pending. The parent
// verifies exit 75 and the independent process sends those exact fixed inputs.
struct StopAfterWrites<M> {
    inner: M,
    stop: bool,
    continuation: bool,
}
impl<M: ModelPort> ModelPort for StopAfterWrites<M> {
    fn limits(&self) -> ModelLimits {
        self.inner.limits()
    }
    fn check_input_attachments(&self, a: &[Attachment], i: &[ImageReference]) -> Result<()> {
        self.inner.check_input_attachments(a, i)
    }
    async fn prepare_request(
        &self,
        run: &RunRecord,
        r: ModelRequest,
    ) -> Result<PreparedModelRequest> {
        let stop_at_continuation = r
            .messages
            .iter()
            .filter(|m| m.role == "provider" && m.data["stop_reason"] == "max_tokens")
            .count()
            == 1;
        let stop_at_view = r
            .messages
            .iter()
            .filter(|m| m.role == "tool" && m.tool.as_deref() == Some("files.write"))
            .count()
            == 2
            && r.messages
                .iter()
                .filter(|m| m.role == "tool" && m.tool.as_deref() == Some("document.view"))
                .count()
                == 3;
        if self.stop
            && if self.continuation {
                stop_at_continuation
            } else {
                stop_at_view
            }
        {
            std::process::exit(75);
        }
        self.inner.prepare_request(run, r).await
    }
    fn input_upper_bound(&self, r: &PreparedModelRequest) -> Result<u64> {
        self.inner.input_upper_bound(r)
    }
    async fn invoke(&self, c: &ToolContext, r: PreparedModelRequest) -> Result<ExternalResult> {
        self.inner.invoke(c, r).await
    }
    async fn lookup(&self, c: &ToolContext, r: PreparedModelRequest) -> Result<Lookup> {
        self.inner.lookup(c, r).await
    }
}

#[tokio::test]
async fn process_worker() {
    let Ok(schema) = std::env::var("RT_WINDOW_SCHEMA") else {
        return;
    };
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    let store = Arc::new(
        PgStore::open(pool, &schema, Capacity::default())
            .await
            .unwrap(),
    );
    let handle: RunHandle =
        serde_json::from_str(&std::env::var("RT_WINDOW_HANDLE").unwrap()).unwrap();
    let data = DataStore::local(
        std::path::Path::new(&std::env::var("RT_WINDOW_ROOT").unwrap()),
        StoragePolicy::default(),
    )
    .unwrap();
    let access = Access::owner(Namespace {
        owner: handle.clone(),
        agent: "main".into(),
    });
    let stop = std::env::var("RT_WINDOW_PHASE").unwrap() == "stop";
    let now: u64 = std::env::var("RT_WINDOW_NOW").unwrap().parse().unwrap();
    let clock = TestClock::default();
    clock.set(now);
    let claim = store
        .claim(
            &handle,
            if stop {
                "first-process"
            } else {
                "second-process"
            },
            now,
            100,
        )
        .await
        .unwrap();
    let resolver = || ImageResolver {
        store: data.clone(),
        policy: FakePolicy::default(),
        accesses: vec![access.clone()],
        limits: ImageLimits::default(),
    };
    let runtime = Runtime {
        store: store.clone(),
        model: StopAfterWrites {
            stop,
            continuation: std::env::var("RT_WINDOW_CONTINUATION").unwrap() == "true",
            inner: ImageModel {
                inner: Gemini::new(model_config(
                    std::env::var("RT_WINDOW_ENDPOINT").unwrap(),
                    handle.clone(),
                ))
                .unwrap(),
                resolver: resolver(),
                runs: store.clone(),
            },
        },
        tools: ToolSet::default()
            .files(data.clone(), access.clone())
            .select(StandardTool::Write)
            .document_view(DocumentView {
                resolver: resolver(),
                external: None,
                runs: store.clone(),
                clock: clock.clone(),
                output_access: access,
                limits: DocumentViewLimits::default(),
            })
            .select(StandardTool::DocumentView)
            .build()
            .unwrap(),
        policy: FakePolicy::default(),
        clock,
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    assert_eq!(
        runtime.drive(&claim).await.unwrap().state,
        RunState::Terminal(Terminal::Succeeded)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_ten_pages_in_pairs_process_exit_and_reopen_fixed_page() {
    pg_page_window(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_page_window_two_continuations_process_exit_and_resume() {
    pg_page_window(true).await;
}

async fn pg_page_window(continuation: bool) {
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("Pg tests never skip");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    let schema = format!("window_{}", uuid_id());
    let store = Arc::new(
        PgStore::open(pool, &schema, Capacity::default())
            .await
            .unwrap(),
    );
    let account = store
        .create_account("tenant", Amount::new(100000, 100000), None)
        .await
        .unwrap();
    let handle = store
        .start(RunSpec::new(
            "tenant",
            "pairs",
            account,
            "Read and save all pages",
        ))
        .await
        .unwrap();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(&schema);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let data = DataStore::local(&root, StoragePolicy::default()).unwrap();
    let access = Access::owner(Namespace {
        owner: handle.clone(),
        agent: "main".into(),
    });
    let file = data
        .write(&access, "input.pdf", None, &pages_pdf())
        .await
        .unwrap();
    let attachment = Attachment::Stored(ImageReference::Workspace {
        namespace: access.namespace().clone(),
        file,
        mime_type: "application/pdf".into(),
    });
    let mut script = Vec::new();
    for first in (1..=9).step_by(2) {
        script.push(call(
            &format!("view-{first}"),
            "document.view",
            json!({"attachment":attachment,"pages":{"first":first,"last":first+1}}),
        ));
        if continuation && first == 1 {
            script.extend([truncated(), truncated()]);
        }
        script.push(call(&format!("save-{first}"),"files.write",json!({"path":format!("parts/{first}.md"),"expected_revision":null,"text":format!("# Pages {first}-{}\nPAGE_{first:02}\nPAGE_{:02}",first+1,first+1)})));
    }
    // Reopen the same saved fixed Workspace version: no original-file RPC.
    script.push(call(
        "reopen",
        "document.view",
        json!({"attachment":attachment,"pages":{"first":1,"last":1}}),
    ));
    script.push(call(
        "save-reopen",
        "files.write",
        json!({"path":"parts/reopened.md","expected_revision":null,"text":"PAGE_01"}),
    ));
    script.push(done());
    let (endpoint, http) = super::document_contracts::sequence_server(script).await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let spawn = |endpoint: String, phase: &str, time: u64| {
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "attachment_window::process_worker",
            "--nocapture",
        ])
        .env("RT_WINDOW_SCHEMA", &schema)
        .env("RT_WINDOW_ROOT", &root)
        .env("RT_WINDOW_HANDLE", serde_json::to_string(&handle).unwrap())
        .env("RT_WINDOW_ENDPOINT", endpoint)
        .env("RT_WINDOW_PHASE", phase)
        .env("RT_WINDOW_CONTINUATION", continuation.to_string())
        .env("RT_WINDOW_NOW", time.to_string());
        tokio::task::spawn_blocking(move || cmd.output().unwrap())
    };
    let first = spawn(endpoint.clone(), "stop", now).await.unwrap();
    assert_eq!(
        first.status.code(),
        Some(75),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let current = store.get(&handle).await.unwrap();
    assert_eq!(
        current
            .checkpoint
            .as_ref()
            .unwrap()
            .messages
            .iter()
            .filter(|m| m.tool.as_deref() == Some("files.write") && m.role == "tool")
            .count(),
        if continuation { 0 } else { 2 }
    );
    if continuation {
        assert_eq!(
            current.checkpoint.as_ref().unwrap().payload["continuations"],
            1
        );
    }
    let second = spawn(endpoint, "resume", now + 101).await.unwrap();
    assert!(
        second.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    let captures = http.await.unwrap();
    assert_eq!(captures.len(), if continuation { 15 } else { 13 });
    for (index, request) in captures.iter().enumerate() {
        let index = if continuation && index > 1 {
            index.saturating_sub(2).max(1)
        } else {
            index
        };
        let parts = request["contents"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|c| c["parts"].as_array().unwrap());
        let payloads = parts
            .filter_map(|p| p["inlineData"]["data"].as_str())
            .map(|s| STANDARD.decode(s).unwrap())
            .collect::<Vec<_>>();
        assert!(payloads.len() <= 4);
        assert!(payloads.iter().map(Vec::len).sum::<usize>() <= 8 * 1024 * 1024);
        let expected: Vec<u32> = if index % 2 == 1 && index < 10 {
            vec![index as u32, index as u32 + 1]
        } else if index == 11 {
            vec![1]
        } else {
            vec![]
        };
        assert_eq!(
            payloads.len(),
            usize::from(!expected.is_empty()),
            "request {index}"
        );
        if let Some(bytes) = payloads.first() {
            let doc = lopdf::Document::load_mem(bytes).unwrap();
            assert_eq!(doc.get_pages().len(), expected.len());
            let content = doc
                .get_pages()
                .values()
                .flat_map(|id| doc.get_page_content(*id).unwrap())
                .collect::<Vec<_>>();
            let text = String::from_utf8(content).unwrap();
            for page in 1..=10 {
                assert_eq!(
                    text.contains(&format!("PAGE_{page:02}")),
                    expected.contains(&page),
                    "request {index}, page {page}"
                );
            }
        }
    }
    let final_run = store.get(&handle).await.unwrap();
    assert_eq!(final_run.state, RunState::Terminal(Terminal::Succeeded));
    let ops = store.list_operations(&handle).await.unwrap();
    assert!(ops.iter().all(|o| o.send_count == 1 && o.settled));
    let view = ops
        .iter()
        .find(|o| {
            o.request.target == "document.view"
                && o.request.input["pages"]["first"] == 1
                && o.request.input["pages"]["last"] == 2
        })
        .unwrap();
    let selected: Vec<Attachment> = serde_json::from_value(match view.result.as_ref().unwrap() {
        ExternalResult::Applied { output, .. } => output["attachments"].clone(),
        _ => panic!(),
    })
    .unwrap();
    assert_eq!(
        selected[0].declaration().digest,
        content_digest(
            &STANDARD
                .decode(
                    captures[1]["contents"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .flat_map(|c| c["parts"].as_array().unwrap())
                        .find_map(|p| p["inlineData"]["data"].as_str())
                        .unwrap()
                )
                .unwrap()
        )
    );
    let snapshot = data.snapshot(&access).await.unwrap();
    assert_eq!(
        snapshot
            .files
            .iter()
            .filter(|f| f.path.starts_with("parts/"))
            .count(),
        6
    );
    for file in snapshot
        .files
        .iter()
        .filter(|f| f.path.starts_with("parts/"))
    {
        let bytes = data
            .read(&access, &file.path, file.revision, 0, file.bytes as usize)
            .await
            .unwrap();
        assert!(String::from_utf8(bytes.bytes).unwrap().contains("PAGE_"));
    }
    std::fs::remove_dir_all(root).unwrap();
}
