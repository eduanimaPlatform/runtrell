// SPDX-License-Identifier: Apache-2.0
//! Shared Memory/Pg contracts for image tools and the ordinary durable driver.
use crate::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use runtrell::{data::*, images::*, standard_tools::*};

pub fn png() -> Vec<u8> {
    // One RGBA pixel, uncompressed DEFLATE, with computed PNG CRCs and Adler32.
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc = !0u32;
        for b in kind.iter().chain(data) {
            crc ^= u32::from(*b);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        out.extend_from_slice(&(!crc).to_be_bytes());
    }
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(
        &mut bytes,
        b"IHDR",
        &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0],
    );
    let pixels = [0u8, 255, 0, 0, 255];
    let mut stream = vec![0x78, 0x01, 1, 5, 0, 250, 255];
    stream.extend_from_slice(&pixels);
    let (mut a, mut b) = (1u32, 0u32);
    for p in pixels {
        a = (a + u32::from(p)) % 65521;
        b = (b + a) % 65521;
    }
    stream.extend_from_slice(&((b << 16) | a).to_be_bytes());
    chunk(&mut bytes, b"IDAT", &stream);
    chunk(&mut bytes, b"IEND", &[]);
    bytes
}
#[derive(Clone)]
pub struct FakeImages {
    pub sends: Arc<AtomicU64>,
    pub data: String,
    pub mime: String,
    pub usage: Usage,
}
impl Default for FakeImages {
    fn default() -> Self {
        Self {
            sends: Arc::default(),
            data: STANDARD.encode(png()),
            mime: "image/png".into(),
            usage: Usage::measured(3, 5, 8),
        }
    }
}
impl ImagePort for FakeImages {
    fn identity(&self) -> String {
        "fake-images-1".into()
    }
    fn check(&self, r: &PreparedImageRequest) -> Result<()> {
        if r.action() == ImageAction::MaskEdit {
            return Err(Error::Unsupported("mask_edit".into()));
        }
        Ok(())
    }
    async fn invoke(&self, _: &ToolContext, r: PreparedImageRequest) -> Result<ImageResponse> {
        self.check(&r)?;
        for image in r.images() {
            assert_eq!(STANDARD.decode(image.data_base64()).unwrap(), png());
        }
        self.sends.fetch_add(1, Ordering::SeqCst);
        Ok(ImageResponse::Received {
            images: vec![EncodedImage {
                mime_type: self.mime.clone(),
                data_base64: self.data.clone(),
            }],
            usage: self.usage.clone(),
            failure: None,
        })
    }
}
pub struct LoseImageReply<T> {
    pub inner: T,
    pub lose: Arc<AtomicBool>,
}
impl<T: Tool> Tool for LoseImageReply<T> {
    fn descriptor(&self) -> ToolDescriptor {
        self.inner.descriptor()
    }
    async fn quote(&self, r: &RunRecord, v: &Value) -> Result<Amount> {
        self.inner.quote(r, v).await
    }
    async fn authorize_result(&self, r: &RunRecord, v: &Value) -> Result<()> {
        self.inner.authorize_result(r, v).await
    }
    async fn invoke(&self, c: &ToolContext, v: Value) -> Result<ExternalResult> {
        let result = self.inner.invoke(c, v).await?;
        if self.lose.swap(false, Ordering::SeqCst) {
            Ok(ExternalResult::Unknown)
        } else {
            Ok(result)
        }
    }
    async fn lookup(&self, c: &ToolContext, v: Value) -> Result<Lookup> {
        self.inner.lookup(c, v).await
    }
}
#[derive(Clone, Default)]
pub struct ImageLoop {
    pub inner: FakeModel,
    pub captures: Arc<Mutex<Vec<Vec<String>>>>,
}
impl ModelPort for ImageLoop {
    fn limits(&self) -> ModelLimits {
        self.inner.limits()
    }
    fn input_upper_bound(&self, r: &PreparedModelRequest) -> Result<u64> {
        assert_eq!(r.request().images.len(), r.resolved_images().len());
        for image in r.resolved_images() {
            assert_eq!(STANDARD.decode(image.data_base64()).unwrap(), png());
        }
        self.inner.input_upper_bound(r)
    }
    async fn invoke(&self, c: &ToolContext, r: PreparedModelRequest) -> Result<ExternalResult> {
        self.captures
            .lock()
            .unwrap()
            .push(r.request().images.iter().map(|i| i.hash().into()).collect());
        let tools: Vec<_> = r.messages.iter().filter(|m| m.role == "tool").collect();
        let reply = match tools.len() {
            0 => FakeModel::calls(vec![ToolCall {
                id: "gen".into(),
                name: "image.generate".into(),
                arguments: json!({"prompt":"a red pixel"}),
            }]),
            1 => FakeModel::calls(vec![ToolCall {
                id: "edit".into(),
                name: "image.edit".into(),
                arguments: json!({"prompt":"change it","references":tools[0].data["image_inputs"]}),
            }]),
            _ => FakeModel::decision(json!({"decision":"answered"})),
        };
        self.inner.replies.lock().unwrap().push_back(reply);
        self.inner.invoke(c, r).await
    }
    async fn lookup(&self, c: &ToolContext, r: PreparedModelRequest) -> Result<Lookup> {
        self.inner.lookup(c, r).await
    }
    fn recovery(&self) -> Option<(String, u64)> {
        self.inner.recovery()
    }
}
fn resolver(
    data: DataStore,
    access: Access,
    limits: ImageLimits,
) -> ImageResolver<DataStore, FakePolicy> {
    ImageResolver {
        store: data,
        policy: FakePolicy::default(),
        accesses: vec![access],
        limits,
    }
}
pub async fn workflow<S: ControlStore + 'static>(store: Arc<S>, data: DataStore) {
    workflow_reopened(store.clone(), data.clone(), store, data).await;
}
pub async fn workflow_reopened<S: ControlStore + 'static>(
    store: Arc<S>,
    data: DataStore,
    reopened: Arc<S>,
    reloaded: DataStore,
) {
    ImageLimits::default().inspect("image/png", &png()).unwrap();
    let account = store
        .create_account("images", Amount::new(10000, 10000), None)
        .await
        .unwrap();
    let mut spec = RunSpec::new(
        "images",
        "workflow",
        account.clone(),
        "draw, inspect and edit",
    );
    let parent = store.start(spec.clone()).await.unwrap();
    let source_access = Access::owner(Namespace {
        owner: parent.clone(),
        agent: "main".into(),
    });
    let file = WorkspaceStore::write(&data, &source_access, "input.png", None, &png())
        .await
        .unwrap();
    let staged = ArtifactStore::stage_named(
        &data,
        &source_access,
        ArtifactSpec {
            id: "initial-image".into(),
            media_type: "image/png".into(),
            class: DataClass::Intermediate,
        },
        &png(),
        0,
    )
    .await
    .unwrap();
    let artifact = ArtifactStore::save(&data, &source_access, staged, 0)
        .await
        .unwrap();
    // Pin inputs before creating the actual Run. The namespace is host-bound to
    // its explicit grant; this second Run cannot silently read the first one.
    spec.request_key = "with-inputs".into();
    spec.parent = Some(parent);
    spec.images = vec![
        ImageReference::Workspace {
            namespace: source_access.namespace().clone(),
            file: file.clone(),
            mime_type: "image/png".into(),
        },
        ImageReference::Artifact(artifact),
    ];
    let mut denied_spec = spec.clone();
    denied_spec.request_key = "denied-images".into();
    let denied_run = store.start(denied_spec).await.unwrap();
    let denied_claim = store.claim(&denied_run, "denied", 0, 100).await.unwrap();
    let fake = FakeModel::scripted(vec![FakeModel::decision(json!({"decision":"answered"}))]);
    let denied_runtime = Runtime {
        store: store.clone(),
        model: ImageModel {
            inner: fake.clone(),
            resolver: resolver(
                data.clone(),
                Access::for_actor(
                    Namespace {
                        owner: denied_run.clone(),
                        agent: "main".into(),
                    },
                    source_access.namespace().clone(),
                ),
                ImageLimits::default(),
            ),
            runs: store.clone(),
        },
        tools: Vec::<BusinessTool>::new(),
        policy: FakePolicy::default(),
        clock: TestClock::default(),
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    assert!(matches!(
        denied_runtime.drive(&denied_claim).await.unwrap().state,
        RunState::Terminal(Terminal::Failed(_))
    ));
    assert!(fake.requests.lock().unwrap().is_empty());
    assert!(store.list_operations(&denied_run).await.unwrap().is_empty());
    let handle = store.start(spec.clone()).await.unwrap();
    let foreign = store.get(&handle).await.unwrap();
    assert_eq!(
        resolver(
            data.clone(),
            Access::for_actor(
                Namespace {
                    owner: handle.clone(),
                    agent: "main".into()
                },
                source_access.namespace().clone()
            ),
            ImageLimits::default()
        )
        .resolve(&foreign, &spec.images)
        .await
        .unwrap_err(),
        Error::PermissionDenied
    );
    let grant = data
        .share(&source_access, &foreign, "main", false, file.revision)
        .await
        .unwrap();
    let access = Access::owner(Namespace {
        owner: handle.clone(),
        agent: "main".into(),
    });
    let resolved = |limits| ImageResolver {
        store: data.clone(),
        policy: FakePolicy::default(),
        accesses: vec![access.clone(), grant.clone()],
        limits,
    };
    // A trusted initial Workspace/Artifact attachment to the actual owning Run
    // is pinned as durable operation input before the ordinary loop starts.
    let claim = store.claim(&handle, "worker", 0, 100).await.unwrap();
    let provider = FakeImages::default();
    let clock = TestClock::default();
    let lose = Arc::new(AtomicBool::new(true));
    let make_tools = |runs: Arc<S>, store: DataStore| {
        let make_tool = |action| ImageTool {
            provider: provider.clone(),
            resolver: ImageResolver {
                store: store.clone(),
                policy: FakePolicy::default(),
                accesses: vec![access.clone(), grant.clone()],
                limits: ImageLimits::default(),
            },
            runs: runs.clone(),
            clock: clock.clone(),
            output_access: access.clone(),
            config: ImageToolConfig {
                action,
                reserve: Amount::new(32, 32),
                timeout: 30,
            },
        };
        let incompatible = ToolSet::<DataStore>::default()
            .image(make_tool(ImageAction::Generate))
            .settings(
                StandardTool::ImageGenerate,
                ToolSettings {
                    stored_output: Some(StoredOutputSettings {
                        inline_bytes: 128,
                        now: 0,
                    }),
                    ..Default::default()
                },
            )
            .build()
            .err()
            .unwrap();
        assert!(incompatible.iter().any(|d| d.code == "SETTING_UNSUPPORTED"));
        let selected = ToolSet::<DataStore>::default()
            .select(StandardTool::ImageGenerate)
            .select(StandardTool::ImageEdit)
            .image(make_tool(ImageAction::Generate))
            .image(make_tool(ImageAction::Edit))
            .build()
            .unwrap();
        selected
            .into_iter()
            .map(|inner| LoseImageReply {
                inner,
                lose: lose.clone(),
            })
            .collect::<Vec<_>>()
    };
    let model = ImageLoop::default();
    let rt = Runtime {
        store: store.clone(),
        model: ImageModel {
            inner: model.clone(),
            resolver: resolved(ImageLimits::default()),
            runs: store.clone(),
        },
        tools: make_tools(store.clone(), data.clone()),
        policy: FakePolicy::default(),
        clock: clock.clone(),
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    assert!(matches!(
        rt.drive(&claim).await.unwrap().state,
        RunState::Waiting(_)
    ));
    assert_eq!(provider.sends.load(Ordering::SeqCst), 1);
    assert_eq!(store.account(&account).await.unwrap().unknown, 1);
    drop(rt);
    let next = reopened
        .claim(&handle, "restarted", 101, 100)
        .await
        .unwrap();
    clock.set(101);
    let rt = Runtime {
        store: reopened.clone(),
        model: ImageModel {
            inner: model.clone(),
            resolver: ImageResolver {
                store: reloaded.clone(),
                policy: FakePolicy::default(),
                accesses: vec![access.clone(), grant.clone()],
                limits: ImageLimits::default(),
            },
            runs: reopened.clone(),
        },
        tools: make_tools(reopened, reloaded),
        policy: FakePolicy::default(),
        clock: clock.clone(),
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    assert_eq!(
        rt.drive(&next).await.unwrap().state,
        RunState::Terminal(Terminal::Succeeded)
    );
    assert_eq!(provider.sends.load(Ordering::SeqCst), 2); // generate once, edit once
    let ops = store.list_operations(&handle).await.unwrap();
    assert_eq!(ops.len(), 5);
    assert!(
        ops.iter()
            .all(|o| o.settled && o.send_count == 1 && o.usage_observations.is_empty())
    );
    assert_eq!(
        store.account(&account).await.unwrap().used,
        Amount::new(28, 28)
    );
    {
        let captures = model.captures.lock().unwrap();
        assert_eq!(captures[0], vec![content_digest(&png()); 2]);
        assert_eq!(captures[1], vec![content_digest(&png()); 3]);
        assert_eq!(captures[2], vec![content_digest(&png()); 4]);
    }
    let raw = serde_json::to_string(&ops).unwrap();
    assert!(!raw.contains(&STANDARD.encode(png())));
    assert!(!raw.contains("data_base64"));
    let own_run = store.get(&handle).await.unwrap();
    let refs = vec![ImageReference::Workspace {
        namespace: source_access.namespace().clone(),
        file,
        mime_type: "image/png".into(),
    }];
    let before = resolved(ImageLimits::default())
        .resolve(&own_run, &refs)
        .await
        .unwrap();
    assert_eq!(before[0].reference().hash(), content_digest(&png()));
    let mut wrong = refs.clone();
    if let ImageReference::Workspace { file, .. } = &mut wrong[0] {
        file.hash = "wrong".into();
    }
    assert!(
        resolved(ImageLimits::default())
            .resolve(&own_run, &wrong)
            .await
            .is_err()
    );
    let denied = FakePolicy::default();
    denied.revoked.store(true, Ordering::SeqCst);
    let denied = ImageResolver {
        store: data.clone(),
        policy: denied,
        accesses: vec![access],
        limits: ImageLimits::default(),
    };
    assert_eq!(
        denied.resolve(&own_run, &refs).await.unwrap_err(),
        Error::PermissionDenied
    );
    let original = ops
        .iter()
        .find(|o| o.request.kind == OperationKind::Model)
        .unwrap();
    let request = serde_json::from_value(original.request.input.clone()).unwrap();
    let prepared = rt.model.prepare_request(&own_run, request).await.unwrap();
    data.revoke(&source_access).await.unwrap();
    let context = ToolContext {
        recovery_proof: None,
        claim: next,
        principal: own_run.spec.principal,
        operation: original.id.clone(),
        attempt: 1,
        deadline: None,
        permission: "model".into(),
        account_binding: "host".into(),
    };
    assert_eq!(
        rt.model.lookup(&context, prepared).await.unwrap_err(),
        Error::PermissionDenied
    );
}
pub async fn billed_failures<S: ControlStore + 'static>(store: Arc<S>) {
    for (index, (mime, data, maximum, known)) in [
        ("image/png", "???".into(), 1024, true),
        ("image/jpeg", STANDARD.encode(png()), 1024, true),
        ("image/png", STANDARD.encode(png()), 16, true),
        ("image/png", STANDARD.encode(png()), 1024, false),
    ]
    .into_iter()
    .enumerate()
    {
        let account = store
            .create_account("images", Amount::new(1000, 1000), None)
            .await
            .unwrap();
        let handle = store
            .start(RunSpec::new(
                "images",
                &format!("failure-{index}"),
                account.clone(),
                "draw",
            ))
            .await
            .unwrap();
        let claim = store.claim(&handle, "worker", 0, 100).await.unwrap();
        let access = Access::owner(Namespace {
            owner: handle.clone(),
            agent: "main".into(),
        });
        let provider = FakeImages {
            mime: mime.into(),
            data,
            usage: if known {
                Usage::measured(3, 5, 8)
            } else {
                Usage::default()
            },
            ..Default::default()
        };
        let tool = ImageTool {
            provider: provider.clone(),
            resolver: resolver(
                DataStore::memory(StoragePolicy::default()),
                access.clone(),
                ImageLimits {
                    max_image_bytes: maximum,
                    ..Default::default()
                },
            ),
            runs: store.clone(),
            clock: TestClock::default(),
            output_access: access,
            config: ImageToolConfig {
                action: ImageAction::Generate,
                reserve: Amount::new(32, 32),
                timeout: 30,
            },
        };
        let rt = Runtime {
            store: store.clone(),
            model: FakeModel::scripted(vec![FakeModel::calls(vec![ToolCall {
                id: "gen".into(),
                name: "image.generate".into(),
                arguments: json!({"prompt":"draw"}),
            }])]),
            tools: vec![tool],
            policy: FakePolicy::default(),
            clock: TestClock::default(),
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let result = rt.drive(&claim).await.unwrap();
        assert_eq!(provider.sends.load(Ordering::SeqCst), 1);
        let op = store
            .list_operations(&handle)
            .await
            .unwrap()
            .into_iter()
            .find(|o| o.request.kind == OperationKind::Tool)
            .unwrap();
        if known {
            assert!(matches!(
                result.state,
                RunState::Terminal(Terminal::Failed(_))
            ));
            assert!(op.settled && matches!(op.result, Some(ExternalResult::Failed { .. })));
            assert_eq!(
                store.account(&account).await.unwrap().used,
                Amount::new(12, 12)
            );
        } else {
            assert!(matches!(result.state, RunState::Waiting(_)));
            assert!(!op.settled);
            assert_eq!(store.account(&account).await.unwrap().unknown, 1);
        }
    }
}
