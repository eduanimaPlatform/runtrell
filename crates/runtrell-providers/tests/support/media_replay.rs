// SPDX-License-Identifier: Apache-2.0
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use runtrell::{
    attachments::*,
    data::{Access, DataStore, Namespace, StoragePolicy, content_digest},
    images::{ImageModel, ImageReference, ImageResolver},
    memory::MemoryStore,
};
use runtrell_testkit::{DecisionFinalizer, DecisionValidator, FakePolicy, TestClock};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct MediaTool {
    image: ImageReference,
}

fn pdf_bytes(page: usize) -> Vec<u8> {
    format!("%PDF-{page}\n").repeat(page * 64).into_bytes()
}

fn pdf(page: usize) -> Attachment {
    let bytes = pdf_bytes(page);
    Attachment::External(ExternalAttachment {
        reference_id: format!("page-{page}"),
        version: "fixed-v1".into(),
        pages: Some(PageRange {
            first: page as u32,
            last: page as u32,
        }),
        mime_type: "application/pdf".into(),
        bytes: bytes.len() as u64,
        digest: content_digest(&bytes),
    })
}

impl Tool for MediaTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            name: "fixture.open".into(),
            version: "1".into(),
            account: "files".into(),
            permission: "read".into(),
            capability: "Open a fixed input.".into(),
            input_schema: json!({"type":"object"}),
            output_schema: json!({"type":"object"}),
            side_effect: false,
            timeout: 10_000,
            max_output_bytes: 8192,
            reserve: Amount::default(),
            quantities: Default::default(),
            conflict_scope: None,
            retry: RetryPolicy::default(),
        }
    }
    async fn invoke(&self, _: &ToolContext, input: Value) -> Result<ExternalResult> {
        let output = match input["kind"].as_str().unwrap() {
            "pdf" => json!({"attachments":[pdf(input["page"].as_u64().unwrap() as usize)]}),
            "image" => json!({"image_inputs":[self.image]}),
            "text" => json!({"text": "recorded findings"}),
            _ => panic!("unexpected test tool input"),
        };
        Ok(ExternalResult::Applied {
            output,
            physical_evidence: None,
            usage: Usage::measured(0, 0, 0),
            external_id: None,
        })
    }
    async fn lookup(&self, _: &ToolContext, _: Value) -> Result<Lookup> {
        Ok(Lookup::Unknown)
    }
}

#[derive(Default)]
struct Resolver {
    authorized: AtomicUsize,
    resolved: AtomicUsize,
    revoke_page_one_after_first_send: bool,
}
impl AttachmentResolver for Resolver {
    fn authorize(
        &self,
        _: &ToolContext,
        _: &str,
        input: &ExternalAttachment,
    ) -> std::result::Result<(), ResolutionError> {
        if self.revoke_page_one_after_first_send
            && input.reference_id == "page-1"
            && self.resolved.load(Ordering::SeqCst) > 0
        {
            return Err(ResolutionError);
        }
        self.authorized.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn resolve<'a>(
        &'a self,
        _: &'a ToolContext,
        _: &'a str,
        input: &'a ExternalAttachment,
    ) -> ResolveFuture<'a> {
        Box::pin(async move {
            self.resolved.fetch_add(1, Ordering::SeqCst);
            Ok(ResolvedAttachment::bytes(
                input.clone(),
                pdf_bytes(input.pages.as_ref().unwrap().first as usize),
            ))
        })
    }
}

fn call(id: &str, kind: &str, page: usize) -> Value {
    json!({"type":"function_call","status":"completed","call_id":id,
        "name":digest(&json!("fixture.open")), "arguments":json!({"kind":kind,"page":page}).to_string()})
}

fn message(text: &str) -> Value {
    json!({"type":"message","role":"assistant","status":"completed",
        "content":[{"type":"output_text","text":text}]})
}

#[derive(Clone, Copy)]
enum Scenario {
    Replay,
    Revoked,
    Compact,
    CountLimit,
    ByteLimit,
    EstimateExceeded,
}

struct RunResult {
    run: RunRecord,
    operations: Vec<Operation>,
    requests: Vec<(String, Value)>,
    resolver: Arc<Resolver>,
    account: Account,
}

async fn run(scenario: Scenario, outputs: Vec<Vec<Value>>) -> RunResult {
    let mut responses = Vec::new();
    for (index, output) in outputs.into_iter().enumerate() {
        responses.push(json!({"object":"response.input_tokens","input_tokens":100}));
        responses.push(json!({"id":format!("resp_{index}"),"model":"gpt-6.2-sol",
            "status":"completed","service_tier":"default","output":output,
            "usage":{"input_tokens":100,"input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0},
                     "output_tokens":8,"total_tokens":108}}));
    }
    if matches!(scenario, Scenario::EstimateExceeded) {
        responses.push(json!({"object":"response.input_tokens","input_tokens":19_999}));
    }
    let minimum = responses.len();
    let (endpoint, server) = raw_server_with_replies(responses, minimum).await;

    let store = Arc::new(MemoryStore::default());
    let account = store
        .create_account("tenant", Amount::new(90_000, 1_000_000), None)
        .await
        .unwrap();
    let mut spec = RunSpec::new("tenant", "media", account.clone(), "Open the inputs.");
    if matches!(scenario, Scenario::Compact | Scenario::CountLimit) {
        spec.compaction = Some(CompactionPolicy {
            min_remaining_tokens: if matches!(scenario, Scenario::Compact) {
                13_000
            } else {
                1
            },
            preserve_progress: false,
            max_without_progress: None,
        });
    }
    let handle = store.start(spec).await.unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let claim = store.claim(&handle, "worker", now, 100_000).await.unwrap();
    let clock = TestClock::default();
    clock.set(now);
    let access = Access::owner(Namespace {
        owner: handle.clone(),
        agent: "files".into(),
    });
    let data = DataStore::memory(StoragePolicy::default());
    let file = data
        .write(
            &access,
            "input/chart.png",
            None,
            &runtrell_testkit::image_contracts::png(),
        )
        .await
        .unwrap();
    let resolver = Arc::new(Resolver {
        revoke_page_one_after_first_send: matches!(scenario, Scenario::Revoked),
        ..Resolver::default()
    });
    let input_cap = if matches!(scenario, Scenario::CountLimit | Scenario::ByteLimit) {
        70_000
    } else {
        20_000
    };
    let mut model = connect_model(
        "openai/gpt-6.2-sol",
        None,
        Connection {
            run: handle.clone(),
            session: None,
            credentials: Arc::new(|_| Ok(ApiKey::new("FIXTURE-KEY"))),
            attachment_resolver: Some(resolver.clone()),
            retry_policy: RetryPolicy::default(),
            timeout_ms: 10_000,
            context_cap: None,
            input_token_cap: Some(input_cap),
            max_output_tokens: Some(64),
            reasoning: None,
        },
    )
    .unwrap();
    model.0.config.endpoint = endpoint;
    // Exercise a parallel tool exchange with the loopback provider.
    model.0.config.capabilities.parallel_tool_calls = true;
    match scenario {
        Scenario::CountLimit => model.0.config.document_limits.max_inputs = 4,
        Scenario::ByteLimit => {
            model.0.config.document_limits.max_total_bytes =
                (2..=5).map(|page| pdf(page).bytes()).sum()
        }
        _ => {}
    }
    let runtime = Runtime {
        store: store.clone(),
        model: ImageModel {
            inner: model,
            resolver: ImageResolver {
                store: data,
                policy: FakePolicy::default(),
                accesses: vec![access.clone()],
                limits: ImageLimits::default(),
            },
            runs: store.clone(),
        },
        tools: vec![MediaTool {
            image: ImageReference::Workspace {
                namespace: access.namespace().clone(),
                file,
                mime_type: "image/png".into(),
            },
        }],
        policy: FakePolicy::default(),
        clock,
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    let run = runtime.drive(&claim).await.unwrap();
    let requests = server
        .await
        .unwrap()
        .into_iter()
        .map(|wire| {
            let (headers, body) = wire.split_once("\r\n\r\n").unwrap();
            (
                headers.lines().next().unwrap().into(),
                serde_json::from_str(body).unwrap(),
            )
        })
        .collect();
    RunResult {
        run,
        operations: store.list_operations(&handle).await.unwrap(),
        requests,
        resolver,
        account: store.account(&account).await.unwrap(),
    }
}

fn prompts(result: &RunResult) -> Vec<&Vec<Value>> {
    let (pairs, remainder) = result.requests.as_chunks::<2>();
    assert!(remainder.is_empty());
    pairs
        .iter()
        .map(|[(count_path, counted), (send_path, sent)]| {
            assert!(count_path.starts_with("POST /test/input_tokens "));
            assert!(send_path.starts_with("POST /test "));
            assert_eq!(counted["input"], sent["input"]);
            sent["input"].as_array().unwrap()
        })
        .collect()
}

fn media(input: &[Value]) -> Vec<&Value> {
    input
        .iter()
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter(|part| matches!(part["type"].as_str(), Some("input_file" | "input_image")))
        .collect()
}

fn model_input_reservation(op: &Operation) -> u64 {
    op.request.reserve.tokens - op.request.input["max_output"].as_u64().unwrap()
}

#[tokio::test]
async fn tool_media_replay_preserves_responses_prefix_order_and_sizes() {
    let result = run(
        Scenario::Replay,
        vec![
            vec![call("first", "pdf", 1), call("chart", "image", 0)],
            vec![call("second", "pdf", 2)],
            vec![call("reread", "pdf", 1)],
            vec![call("save", "text", 0)],
            vec![message("{\"decision\":\"answered\"}")],
        ],
    )
    .await;
    assert_eq!(result.run.state, RunState::Terminal(Terminal::Succeeded));
    let inputs = prompts(&result);
    assert_eq!(inputs.len(), 5);
    for pair in inputs.windows(2) {
        assert!(
            pair[1].starts_with(pair[0]),
            "the previous wire input must be a prefix"
        );
    }
    let first = media(inputs[1]);
    let second = media(inputs[2]);
    assert_eq!(first.len(), 2);
    let pdf_data = |page| {
        format!(
            "data:application/pdf;base64,{}",
            STANDARD.encode(pdf_bytes(page))
        )
    };
    assert_eq!(first[0]["file_data"], pdf_data(1));
    assert_eq!(first[1]["type"], "input_image");
    assert_eq!(second.len(), 3);
    assert_eq!(second[2]["file_data"], pdf_data(2));
    assert_eq!(media(inputs[3]), second);
    assert_eq!(media(inputs[4]), second);
    let tail = &inputs[1][inputs[0].len()..];
    assert_eq!(tail[2]["call_id"], "first");
    assert_eq!(tail[3]["call_id"], "chart");
    assert_eq!(tail[4]["role"], "user");
    assert_eq!(result.resolver.resolved.load(Ordering::SeqCst), 7);
    assert_eq!(result.resolver.authorized.load(Ordering::SeqCst), 7);

    let bounds = result
        .operations
        .iter()
        .filter(|op| op.request.kind == OperationKind::Model)
        .map(model_input_reservation)
        .collect::<Vec<_>>();
    assert_eq!(bounds.len(), 5);
    assert!(bounds.windows(2).all(|p| p[0] < p[1]), "{bounds:?}");
    assert!(
        bounds[2] - bounds[1] >= pdf_bytes(2).len() as u64,
        "{bounds:?}"
    );
    assert!(bounds.iter().all(|n| *n < 20_000), "{bounds:?}");
    assert_eq!(result.account.reserved, Amount::default());
}

#[tokio::test]
async fn tool_media_replay_revocation_settles_without_another_provider_request() {
    let result = run(
        Scenario::Revoked,
        vec![vec![call("first", "pdf", 1)], vec![call("save", "text", 0)]],
    )
    .await;
    assert_eq!(prompts(&result).len(), 2);
    let failed = result
        .operations
        .iter()
        .find(|op| {
            matches!(
                &op.result, Some(ExternalResult::Failed { code, usage, .. })
                    if code == "ATTACHMENT_NOT_SENT" && usage.amount() == Ok(Amount::default())
            )
        })
        .expect("revoked replay must fail closed");
    assert!(failed.settled);
    assert_eq!(result.resolver.resolved.load(Ordering::SeqCst), 1);
    assert_eq!(result.account.reserved, Amount::default());
}

#[tokio::test]
async fn compaction_replaces_old_media_with_reference_text_and_keeps_pending_media() {
    let summary = "Page one was opened. Continue with page two.";
    let result = run(
        Scenario::Compact,
        vec![
            vec![call("first", "pdf", 1)],
            vec![call("second", "image", 0)],
            vec![message(summary)],
            vec![message("{\"decision\":\"answered\"}")],
        ],
    )
    .await;
    assert_eq!(result.run.state, RunState::Terminal(Terminal::Succeeded));
    let inputs = prompts(&result);
    assert_eq!(media(inputs[1]).len(), 1);
    assert!(media(inputs[2]).is_empty());
    let resumed = json!(inputs[3]).to_string();
    assert!(
        resumed.contains(summary)
            && resumed.contains("references only")
            && resumed.contains("page-1"),
        "{resumed}"
    );
    let pending = media(inputs[3]);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["type"], "input_image");
    assert_eq!(result.resolver.resolved.load(Ordering::SeqCst), 1);
    assert_eq!(
        result
            .operations
            .iter()
            .filter(|op| op.request.kind == OperationKind::Compaction)
            .count(),
        1
    );
}

#[tokio::test]
async fn measured_media_input_above_the_reservation_never_sends_inference() {
    let result = run(
        Scenario::EstimateExceeded,
        vec![vec![call("first", "pdf", 1)]],
    )
    .await;
    assert_eq!(result.requests.len(), 3);
    assert!(result.requests[2].0.starts_with("POST /test/input_tokens "));
    assert_eq!(
        media(result.requests[2].1["input"].as_array().unwrap()).len(),
        1
    );
    assert!(result.operations.iter().any(|op| op.settled && matches!(
        &op.result, Some(ExternalResult::Failed { code, usage, .. })
            if code == "INPUT_TOKEN_ESTIMATE_EXCEEDED" && usage.amount() == Ok(Amount::default())
    )));
    assert_eq!(result.account.reserved, Amount::default());
}

#[tokio::test]
async fn retained_media_overflow_demotes_oldest_without_compaction_and_reopens_in_place() {
    for scenario in [Scenario::CountLimit, Scenario::ByteLimit] {
        let outputs = (1..=5)
            .map(|page| vec![call(&format!("open-{page}"), "pdf", page)])
            .chain([
                vec![call("reopen", "pdf", 1)],
                vec![message("{\"decision\":\"answered\"}")],
            ])
            .collect();
        let result = run(scenario, outputs).await;
        assert_eq!(result.run.state, RunState::Terminal(Terminal::Succeeded));
        assert!(
            result
                .operations
                .iter()
                .all(|op| op.request.kind != OperationKind::Compaction)
        );
        let inputs = prompts(&result);
        assert!(
            inputs[..5]
                .windows(2)
                .all(|pair| pair[1].starts_with(pair[0]))
        );
        let pages: [&[usize]; 7] = [
            &[],
            &[1],
            &[1, 2],
            &[1, 2, 3],
            &[1, 2, 3, 4],
            &[2, 3, 4, 5],
            &[3, 4, 5, 1],
        ];
        for (input, active_pages) in inputs.iter().zip(pages) {
            let retained = media(input);
            assert_eq!(retained.len(), active_pages.len());
            for (item, page) in retained.iter().zip(active_pages) {
                assert_eq!(
                    item["file_data"],
                    format!(
                        "data:application/pdf;base64,{}",
                        STANDARD.encode(pdf_bytes(*page))
                    )
                );
            }
        }
        assert_eq!(
            inputs[6].last().unwrap()["content"][0]["type"],
            "input_file"
        );
        let original = inputs[1]
            .iter()
            .find(|item| item["type"] == "function_call_output")
            .unwrap();
        for input in &inputs[5..] {
            assert!(input.contains(original));
        }
        let reopened = result
            .operations
            .iter()
            .find(|op| op.request.key == "model:7")
            .unwrap();
        let mut request: ModelRequest =
            serde_json::from_value(reopened.request.input.clone()).unwrap();
        let expected = request.media_slots().unwrap();
        request
            .messages
            .insert(0, Message::text("system", "Role instructions"));
        request
            .messages
            .insert(0, Message::text("developer", "Additional instructions"));
        assert_eq!(request.media_slots().unwrap()[2..], expected);
        assert_eq!(result.resolver.resolved.load(Ordering::SeqCst), 18);
    }
}
