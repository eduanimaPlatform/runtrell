// SPDX-License-Identifier: Apache-2.0
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use runtrell::{data::*, images::*, memory::MemoryStore};
use runtrell_testkit::{
    DecisionFinalizer, DecisionValidator, FakePolicy, TestClock, image_contracts::png,
};
use std::sync::Arc;

fn image_config(url: String) -> Config {
    let mut c = config(url);
    c.capabilities.images = ImageCapabilities {
        input: true,
        generation: true,
        reference_edit: true,
        mask_edit: false,
    };
    c.max_request_bytes = c.image_limits.max_request_bytes;
    c.max_response_bytes = c.image_limits.max_response_bytes;
    c.capabilities.limits.price = Some(Price {
        model: "host-model".into(),
        input: 1_000_000,
        cached: 1_000_000,
        output: 1_000_000,
        reasoning: 1_000_000,
        image_input: 1_000_000,
        image_output: 1_000_000,
        cached_image_input: 1_000_000,
    });
    c
}
fn image_response() -> Value {
    json!({"modelVersion":"host-model","candidates":[{"finishReason":"STOP","content":{"role":"model","parts":[{"inlineData":{"mimeType":"image/png","data":STANDARD.encode(png())}}]}}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":5,"thoughtsTokenCount":0,"promptTokensDetails":[{"modality":"IMAGE","tokenCount":2}],"candidatesTokensDetails":[{"modality":"IMAGE","tokenCount":5}]}})
}
type TestTool = ImageTool<GeminiImages, DataStore, FakePolicy, MemoryStore, TestClock>;
async fn fixture(mut c: Config, action: ImageAction) -> (TestTool, ToolContext) {
    let store = Arc::new(MemoryStore::default());
    let account = store
        .create_account("tenant", Amount::new(10000, 10000), None)
        .await
        .unwrap();
    let run = store
        .start(RunSpec::new("tenant", "images", account, "draw"))
        .await
        .unwrap();
    let claim = store.claim(&run, "worker", 0, 100).await.unwrap();
    c.run = run.clone();
    let access = Access::owner(Namespace {
        owner: run,
        agent: "main".into(),
    });
    let tool = ImageTool {
        provider: GeminiImages::new(c).unwrap(),
        resolver: ImageResolver {
            store: DataStore::memory(StoragePolicy::default()),
            policy: FakePolicy::default(),
            accesses: vec![access.clone()],
            limits: ImageLimits::default(),
        },
        runs: store,
        clock: TestClock::default(),
        output_access: access,
        config: ImageToolConfig {
            action,
            reserve: Amount::new(2048, 2048),
            timeout: 30,
        },
    };
    let context = ToolContext {
        claim,
        permission: tool.descriptor().permission,
        account_binding: "images".into(),
        ..context()
    };
    (tool, context)
}
#[tokio::test]
async fn generation_reference_edit_wire_usage_and_saved_lookup() {
    for action in [ImageAction::Generate, ImageAction::Edit] {
        let (url, task) = server(200, "", image_response()).await;
        let (tool, ctx) = fixture(image_config(url), action).await;
        let mut request = json!({"prompt":"draw"});
        if action == ImageAction::Edit {
            let staged = ArtifactStore::stage_named(
                &tool.resolver.store,
                &tool.output_access,
                ArtifactSpec {
                    id: "input".into(),
                    media_type: "image/png".into(),
                    class: DataClass::Intermediate,
                },
                &png(),
                0,
            )
            .await
            .unwrap();
            let reference =
                ArtifactStore::save(&tool.resolver.store, &tool.output_access, staged, 0)
                    .await
                    .unwrap();
            request["references"] = json!([ImageReference::Artifact(reference)]);
        }
        let result = tool.invoke(&ctx, request.clone()).await.unwrap();
        let ExternalResult::Applied { output, usage, .. } = &result else {
            panic!("expected saved image")
        };
        assert_eq!(usage.input, Some(3));
        assert_eq!(usage.output, Some(5));
        assert_eq!(usage.image_input, Some(2));
        assert_eq!(usage.image_output, Some(5));
        assert_eq!(
            usage.quantities["image_input_count"],
            u64::from(action == ImageAction::Edit)
        );
        assert_eq!(usage.quantities["image_output_count"], 1);
        assert_eq!(usage.micro_usd, Some(8));
        assert_eq!(
            output["image_inputs"][0]["Artifact"]["hash"],
            content_digest(&png())
        );
        assert_eq!(
            tool.lookup(&ctx, request.clone()).await.unwrap(),
            Lookup::Found(result.clone())
        );
        assert_eq!(tool.invoke(&ctx, request.clone()).await.unwrap(), result);
        tool.resolver
            .policy
            .revoked
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            tool.lookup(&ctx, request).await.unwrap_err(),
            Error::PermissionDenied
        );
        let sent = task.await.unwrap();
        assert_eq!(sent.len(), 1);
        let wire: Value = serde_json::from_str(sent[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(
            wire["generationConfig"]["responseModalities"],
            json!(["TEXT", "IMAGE"])
        );
        assert_eq!(wire["contents"][0]["parts"][0]["text"], "draw");
        if action == ImageAction::Edit {
            assert_eq!(
                wire["contents"][0]["parts"][1]["inlineData"]["data"],
                STANDARD.encode(png())
            );
        }
    }
}
#[tokio::test]
async fn image_errors_paid_invalid_unknown_and_no_redirect_or_retry() {
    for (status, body, expected) in [
        (
            401,
            json!({"error":{"message":"TEST-KEY-DO-NOT-PRINT"}}),
            "authentication",
        ),
        (400, json!({"error":{}}), "invalid_request"),
        (429, json!({"error":{}}), "rate_limit"),
        (402, json!({"error":{}}), "spending_limit"),
        (500, json!({"error":{}}), "unclassified"),
        (307, json!({}), "redirect"),
    ] {
        let (url, task) = server(
            status,
            "Location: http://127.0.0.1:1/never\r\nRetry-After: 1\r\n",
            body,
        )
        .await;
        let (tool, ctx) = fixture(image_config(url), ImageAction::Generate).await;
        let ExternalResult::Failed { code, usage, .. } =
            tool.invoke(&ctx, json!({"prompt":"draw"})).await.unwrap()
        else {
            panic!("expected failed")
        };
        assert!(code.contains(expected));
        assert!(!code.contains("TEST-KEY"));
        assert_eq!(usage.input, None);
        assert_eq!(task.await.unwrap().len(), 1);
    }
    for (field, value) in [
        ("data", json!("bad base64")),
        ("mimeType", json!("image/jpeg")),
    ] {
        let mut body = image_response();
        body["candidates"][0]["content"]["parts"][0]["inlineData"][field] = value;
        let (url, task) = server(200, "", body).await;
        let (tool, ctx) = fixture(image_config(url), ImageAction::Generate).await;
        let ExternalResult::Failed { usage, .. } =
            tool.invoke(&ctx, json!({"prompt":"draw"})).await.unwrap()
        else {
            panic!("expected billed failure")
        };
        assert_eq!(usage.micro_usd, Some(8));
        assert_eq!(task.await.unwrap().len(), 1);
    }
    let (url, task) = raw_server("HTTP/1.1 200 OK\r\nContent-Length: 200\r\n\r\n{}".into()).await;
    let (tool, ctx) = fixture(image_config(url), ImageAction::Generate).await;
    assert_eq!(
        tool.invoke(&ctx, json!({"prompt":"draw"})).await.unwrap(),
        ExternalResult::Unknown
    );
    assert_eq!(task.await.unwrap().len(), 1);
}
#[tokio::test]
async fn unsupported_capabilities_limits_and_authorization_send_nothing() {
    let (url, task) = server(200, "", image_response()).await;
    let mut c = image_config(url.clone());
    c.capabilities.images.generation = false;
    let (tool, ctx) = fixture(c, ImageAction::Generate).await;
    assert!(matches!(
        tool.invoke(&ctx, json!({"prompt":"draw"})).await,
        Err(Error::Unsupported(_))
    ));
    let (mut tool, ctx) = fixture(image_config(url.clone()), ImageAction::Generate).await;
    assert!(
        tool.invoke(&ctx, json!({"prompt":"x".repeat(65537)}))
            .await
            .is_err()
    );
    tool.resolver
        .policy
        .revoked
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        tool.invoke(&ctx, json!({"prompt":"draw"}))
            .await
            .unwrap_err(),
        Error::PermissionDenied
    );
    tool.config.action = ImageAction::Edit;
    assert!(
        tool.invoke(&ctx, json!({"prompt":"edit","references":[]}))
            .await
            .is_err()
    );
    let mut c = image_config(url);
    c.capabilities.images.mask_edit = true;
    assert!(matches!(GeminiImages::new(c), Err(Error::Unsupported(_))));
    assert!(task.await.unwrap().is_empty());
}

#[tokio::test]
async fn unsupported_image_input_and_reference_edit_and_revoked_send_are_preflight_errors() {
    let (url, task) = server(200, "", image_response()).await;
    let mut cfg = image_config(url);
    cfg.capabilities.images.reference_edit = false;
    let (tool, ctx) = fixture(cfg.clone(), ImageAction::Edit).await;
    let staged = ArtifactStore::stage_named(
        &tool.resolver.store,
        &tool.output_access,
        ArtifactSpec {
            id: "reference".into(),
            media_type: "image/png".into(),
            class: DataClass::Intermediate,
        },
        &png(),
        0,
    )
    .await
    .unwrap();
    let reference = ImageReference::Artifact(
        ArtifactStore::save(&tool.resolver.store, &tool.output_access, staged, 0)
            .await
            .unwrap(),
    );
    assert!(matches!(
        tool.invoke(
            &ctx,
            json!({"prompt":"edit","references":[reference.clone()]})
        )
        .await,
        Err(Error::Unsupported(_))
    ));
    cfg.run = ctx.claim.run.clone();
    cfg.capabilities.images.input = false;
    let model = ImageModel {
        inner: Gemini::new(cfg).unwrap(),
        resolver: ImageResolver {
            store: tool.resolver.store.clone(),
            policy: FakePolicy::default(),
            accesses: vec![tool.output_access.clone()],
            limits: ImageLimits::default(),
        },
        runs: tool.runs.clone(),
    };
    let run = tool.runs.get(&ctx.claim.run).await.unwrap();
    let prepared = model
        .prepare_request(
            &run,
            ModelRequest {
                model: "host-model".into(),
                messages: vec![Message::text("user", "inspect")],
                tools: vec![],
                max_output: 128,
                metadata: Value::Null,
                images: vec![reference],
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        model.input_upper_bound(&prepared),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        model.invoke(&ctx, prepared.clone()).await,
        Err(Error::Unsupported(_))
    ));
    model
        .resolver
        .policy
        .revoked
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        model.invoke(&ctx, prepared).await.unwrap_err(),
        Error::PermissionDenied
    );
    assert!(task.await.unwrap().is_empty());
}

#[tokio::test]
async fn image_count_byte_and_transport_limits_preserve_paid_usage_or_prevent_send() {
    let mut excessive = image_response();
    let image = excessive["candidates"][0]["content"]["parts"][0].clone();
    excessive["candidates"][0]["content"]["parts"] = json!(vec![image; 5]);
    let (url, task) = server(200, "", excessive).await;
    let (tool, ctx) = fixture(image_config(url), ImageAction::Generate).await;
    let ExternalResult::Failed { usage, .. } =
        tool.invoke(&ctx, json!({"prompt":"draw"})).await.unwrap()
    else {
        panic!("expected count failure")
    };
    assert_eq!(usage.micro_usd, Some(8));
    assert_eq!(usage.quantities["image_output_count"], 5);
    assert_eq!(task.await.unwrap().len(), 1);

    let (url, task) = server(200, "", image_response()).await;
    let mut c = image_config(url);
    c.image_limits.max_image_bytes = 16;
    let (tool, ctx) = fixture(c, ImageAction::Generate).await;
    let ExternalResult::Failed { usage, .. } =
        tool.invoke(&ctx, json!({"prompt":"draw"})).await.unwrap()
    else {
        panic!("expected byte failure")
    };
    assert_eq!(usage.micro_usd, Some(8));
    assert_eq!(task.await.unwrap().len(), 1);

    let (url, task) = server(200, "", image_response()).await;
    let mut c = image_config(url);
    c.image_limits.max_image_bytes = png().len();
    c.image_limits.max_images = 1;
    let wire_limit = png().len().div_ceil(3) * 4 + 65536;
    c.image_limits.max_request_bytes = wire_limit;
    c.image_limits.max_response_bytes = wire_limit;
    c.max_request_bytes = wire_limit;
    c.max_response_bytes = wire_limit;
    let (tool, ctx) = fixture(c, ImageAction::Edit).await;
    let staged = ArtifactStore::stage_named(
        &tool.resolver.store,
        &tool.output_access,
        ArtifactSpec {
            id: "reference".into(),
            media_type: "image/png".into(),
            class: DataClass::Intermediate,
        },
        &png(),
        0,
    )
    .await
    .unwrap();
    let reference = ArtifactStore::save(&tool.resolver.store, &tool.output_access, staged, 0)
        .await
        .unwrap();
    let references = json!([ImageReference::Artifact(reference)]);
    assert!(
        tool.invoke(
            &ctx,
            json!({"prompt":"x".repeat(65536),"references":references})
        )
        .await
        .is_err()
    );
    assert!(
        tool.invoke(
            &ctx,
            json!({"prompt":"edit","references":vec![references[0].clone();5]})
        )
        .await
        .is_err()
    );
    assert!(task.await.unwrap().is_empty());
}

async fn model_image_wire<T: ModelPort>(make: fn(Config) -> Result<T>, kind: &str) {
    let mut body = response(kind, "completed", false);
    match kind {
        "gemini" => {
            body["modelVersion"] = json!("host-model");
            body["candidates"][0]["content"]["parts"] =
                json!([{"text":"{\"decision\":\"answered\"}"}]);
            body["usageMetadata"] =
                json!({"promptTokenCount":3,"candidatesTokenCount":1,"thoughtsTokenCount":0});
        }
        "openai" => {
            body["model"] = json!("host-model");
            body["output"][0]["content"][0]["text"] = json!("{\"decision\":\"answered\"}");
            body["usage"] = json!({"input_tokens":3,"output_tokens":1});
        }
        _ => {
            body["model"] = json!("host-model");
            body["choices"][0]["message"]["content"] = json!("{\"decision\":\"answered\"}");
            body["usage"] = json!({"prompt_tokens":3,"completion_tokens":1});
        }
    }
    let store = Arc::new(MemoryStore::default());
    let account = store
        .create_account("tenant", Amount::new(10000, 10000), None)
        .await
        .unwrap();
    let parent = store
        .start(RunSpec::new("tenant", "source", account.clone(), "source"))
        .await
        .unwrap();
    let data = DataStore::memory(StoragePolicy::default());
    let owner = Access::owner(Namespace {
        owner: parent.clone(),
        agent: "main".into(),
    });
    let file = WorkspaceStore::write(&data, &owner, "input.png", None, &png())
        .await
        .unwrap();
    let staged = ArtifactStore::stage_named(
        &data,
        &owner,
        ArtifactSpec {
            id: "image".into(),
            media_type: "image/png".into(),
            class: DataClass::Intermediate,
        },
        &png(),
        0,
    )
    .await
    .unwrap();
    let artifact = ArtifactStore::save(&data, &owner, staged, 0).await.unwrap();
    let mut spec = RunSpec::new("tenant", "view", account.clone(), "inspect image");
    spec.parent = Some(parent);
    spec.images = vec![
        ImageReference::Artifact(artifact),
        ImageReference::Workspace {
            namespace: owner.namespace().clone(),
            file: file.clone(),
            mime_type: "image/png".into(),
        },
    ];
    let handle = store.start(spec).await.unwrap();
    let run = store.get(&handle).await.unwrap();
    let grant = data
        .share(&owner, &run, "main", false, file.revision)
        .await
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claim = store.claim(&handle, "worker", now, 100).await.unwrap();
    let clock = TestClock::default();
    clock.set(now);
    let (url, task) = server(200, "", body).await;
    let mut cfg = image_config(url);
    cfg.run = handle.clone();
    let rt = Runtime {
        store: store.clone(),
        model: ImageModel {
            inner: make(cfg).unwrap(),
            resolver: ImageResolver {
                store: data,
                policy: FakePolicy::default(),
                accesses: vec![grant],
                limits: ImageLimits::default(),
            },
            runs: store.clone(),
        },
        tools: Vec::<runtrell_testkit::BusinessTool>::new(),
        policy: FakePolicy::default(),
        clock,
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    assert_eq!(
        rt.drive(&claim).await.unwrap().state,
        RunState::Terminal(Terminal::Succeeded)
    );
    let sent = task.await.unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].matches(&STANDARD.encode(png())).count(), 2);
    let operations = store.list_operations(&handle).await.unwrap();
    assert_eq!(
        operations[0].request.input["images"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        !operations[0]
            .request
            .input
            .to_string()
            .contains(&STANDARD.encode(png()))
    );
}
#[tokio::test]
async fn normal_driver_delivers_authorized_images_to_provider_wire() {
    model_image_wire(Gemini::new, "gemini").await;
    #[cfg(feature = "openai")]
    model_image_wire(OpenAi::new, "openai").await;
    #[cfg(feature = "openrouter")]
    model_image_wire(OpenRouter::new, "openrouter").await;
}

#[tokio::test]
async fn pg_runtime_http_generation_reinput_edit_and_saved_reply_loss() {
    use runtrell::standard_tools::{StandardTool, ToolSet};
    use runtrell_postgres::PgStore;
    use runtrell_testkit::image_contracts::{ImageLoop, LoseImageReply};
    use std::sync::atomic::{AtomicBool, Ordering};
    let url =
        std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("image Pg integration never skips");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    let tick = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let schema = format!("http_images_{}", tick.as_nanos());
    let runs = Arc::new(
        PgStore::open(pool.clone(), &schema, Capacity::default())
            .await
            .unwrap(),
    );
    let account = runs
        .create_account("tenant", Amount::new(10000, 10000), None)
        .await
        .unwrap();
    let handle = runs
        .start(RunSpec::new(
            "tenant",
            "http-workflow",
            account.clone(),
            "generate, view and edit",
        ))
        .await
        .unwrap();
    let claim = runs
        .claim(&handle, "worker", tick.as_secs(), 100)
        .await
        .unwrap();
    let access = Access::owner(Namespace {
        owner: handle.clone(),
        agent: "main".into(),
    });
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(&schema);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let data = DataStore::local(&root, StoragePolicy::default()).unwrap();
    let (endpoint, http) = server_at_least(200, "", image_response(), 2).await;
    let mut cfg = image_config(endpoint);
    cfg.run = handle.clone();
    let clock = TestClock::default();
    clock.set(tick.as_secs());
    let lose = Arc::new(AtomicBool::new(true));
    let make_runtime = |runs: Arc<PgStore>, data: DataStore| {
        let resolve = || ImageResolver {
            store: data.clone(),
            policy: FakePolicy::default(),
            accesses: vec![access.clone()],
            limits: ImageLimits::default(),
        };
        let tool = |action| ImageTool {
            provider: GeminiImages::new(cfg.clone()).unwrap(),
            resolver: resolve(),
            runs: runs.clone(),
            clock: clock.clone(),
            output_access: access.clone(),
            config: ImageToolConfig {
                action,
                reserve: Amount::new(2048, 2048),
                timeout: 30,
            },
        };
        let selected = ToolSet::<DataStore>::default()
            .select(StandardTool::ImageGenerate)
            .select(StandardTool::ImageEdit)
            .image(tool(ImageAction::Generate))
            .image(tool(ImageAction::Edit))
            .build()
            .unwrap();
        Runtime {
            store: runs.clone(),
            model: ImageModel {
                inner: ImageLoop::default(),
                resolver: resolve(),
                runs,
            },
            tools: selected
                .into_iter()
                .map(|inner| LoseImageReply {
                    inner,
                    lose: lose.clone(),
                })
                .collect::<Vec<_>>(),
            policy: FakePolicy::default(),
            clock: clock.clone(),
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        }
    };
    let rt = make_runtime(runs.clone(), data);
    assert!(matches!(
        rt.drive(&claim).await.unwrap().state,
        RunState::Waiting(_)
    ));
    assert!(!lose.load(Ordering::SeqCst));
    assert_eq!(runs.account(&account).await.unwrap().unknown, 1);
    drop(rt);
    let independent = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    let reopened = Arc::new(
        PgStore::open(independent, &schema, Capacity::default())
            .await
            .unwrap(),
    );
    let next = reopened
        .claim(&handle, "reopened", tick.as_secs() + 101, 100)
        .await
        .unwrap();
    clock.set(tick.as_secs() + 101);
    let rt = make_runtime(
        reopened.clone(),
        DataStore::local(&root, StoragePolicy::default()).unwrap(),
    );
    assert_eq!(
        rt.drive(&next).await.unwrap().state,
        RunState::Terminal(Terminal::Succeeded)
    );
    let wire = http.await.unwrap();
    assert_eq!(wire.len(), 2);
    assert_eq!(wire[0].matches(&STANDARD.encode(png())).count(), 0);
    assert_eq!(wire[1].matches(&STANDARD.encode(png())).count(), 1);
    let entries: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {schema}.budget_entries WHERE settled AND NOT unknown"
    ))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(entries, 5);
    let ops = reopened.list_operations(&handle).await.unwrap();
    assert!(ops.iter().all(|o| o.settled && o.send_count == 1));
    assert_eq!(ops.len(), 5);
    assert_eq!(
        reopened.account(&account).await.unwrap().used,
        Amount::new(28, 28)
    );
    std::fs::remove_dir_all(root).unwrap();
}
