// SPDX-License-Identifier: Apache-2.0
use super::*;
use runtrell::attachments::*;
use runtrell::data::content_digest;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const PAYLOAD: &[u8] = b"%PDF-SECRET-DOCUMENT-CONTENT";
const SECRET_URL: &str = "https://files.example.test/fixed.pdf?signature=SECRET-URL";

#[cfg(feature = "gemini")]
struct DocumentModel(Gemini);
#[cfg(feature = "gemini")]
impl ModelPort for DocumentModel {
    fn limits(&self) -> ModelLimits {
        self.0.limits()
    }
    fn input_upper_bound(&self, r: &PreparedModelRequest) -> Result<u64> {
        self.0.input_upper_bound(r)
    }
    async fn prepare_request(
        &self,
        _: &RunRecord,
        mut request: ModelRequest,
    ) -> Result<PreparedModelRequest> {
        // Host fixes declarations before quote; restoration keeps the saved input.
        if request.attachments.is_empty() {
            request.attachments = declared().attachments;
        }
        PreparedModelRequest::text(request)
    }
    async fn invoke(&self, c: &ToolContext, r: PreparedModelRequest) -> Result<ExternalResult> {
        self.0.invoke(c, r).await
    }
    async fn lookup(&self, c: &ToolContext, r: PreparedModelRequest) -> Result<Lookup> {
        self.0.lookup(c, r).await
    }
}
struct Resolver {
    calls: AtomicUsize,
    url: bool,
    mismatch: bool,
}
impl AttachmentResolver for Resolver {
    fn authorize(
        &self,
        _: &ToolContext,
        _: &str,
        _: &ExternalAttachment,
    ) -> std::result::Result<(), ResolutionError> {
        Ok(())
    }
    fn resolve<'a>(
        &'a self,
        _: &'a ToolContext,
        _: &'a str,
        _: &'a ExternalAttachment,
    ) -> ResolveFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.url {
                let mut target = declared().attachments[0].declaration();
                if self.mismatch {
                    target.version = "different-version".into();
                }
                let call = self.calls.load(Ordering::SeqCst);
                Ok(ResolvedAttachment::url(
                    target,
                    if call == 1 {
                        SECRET_URL.to_owned()
                    } else {
                        format!("{SECRET_URL}-{call}")
                    },
                ))
            } else {
                Ok(ResolvedAttachment::bytes(
                    declared().attachments[0].declaration(),
                    if self.mismatch {
                        b"wrong".to_vec()
                    } else {
                        PAYLOAD.to_vec()
                    },
                ))
            }
        })
    }
}
fn declared() -> ModelRequest {
    let mut r = request();
    r.attachments = vec![Attachment::External(ExternalAttachment {
        reference_id: "opaque-fixed-version".into(),
        version: "v1".into(),
        pages: None,
        mime_type: "application/pdf".into(),
        bytes: PAYLOAD.len() as u64,
        digest: content_digest(PAYLOAD),
    })];
    r
}

#[cfg(feature = "gemini")]
#[tokio::test]
async fn document_errors_debug_trace_and_provider_echoes_are_redacted() {
    use std::{io::Write, sync::Mutex};
    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let capture = Capture(Arc::default());
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    for url in [false, true] {
        let mut body = response("gemini", "completed", false);
        use base64::Engine;
        body["candidates"][0]["content"]["parts"] = json!([{"text":if url { SECRET_URL.to_owned() } else { format!("{} {}",String::from_utf8_lossy(PAYLOAD),base64::engine::general_purpose::STANDARD.encode(PAYLOAD)) }}]);
        let (endpoint, http) = server(200, "", body).await;
        let mut c = config(endpoint);
        c.capabilities.documents.pdf = DocumentTransports {
            url: true,
            inline_bytes: true,
        };
        c.attachment_resolver = Some(Arc::new(Resolver {
            calls: AtomicUsize::new(0),
            url,
            mismatch: false,
        }));
        let model = Gemini::new(c.clone()).unwrap();
        let result = model.invoke(&context(), text(declared())).await.unwrap();
        let output = format!("{result:?}");
        if url {
            assert!(!output.contains(SECRET_URL));
        } else {
            assert!(!output.contains("SECRET-DOCUMENT-CONTENT"));
            assert!(!output.contains(&base64::engine::general_purpose::STANDARD.encode(PAYLOAD)));
        }
        struct Refuse;
        impl AttachmentResolver for Refuse {
            fn authorize(
                &self,
                _: &ToolContext,
                _: &str,
                _: &ExternalAttachment,
            ) -> std::result::Result<(), ResolutionError> {
                Ok(())
            }
            fn resolve<'a>(
                &'a self,
                _: &'a ToolContext,
                _: &'a str,
                _: &'a ExternalAttachment,
            ) -> ResolveFuture<'a> {
                Box::pin(async { Err(ResolutionError) })
            }
        }
        c.attachment_resolver = Some(Arc::new(Refuse));
        let error = Gemini::new(c)
            .unwrap()
            .invoke(&context(), text(declared()))
            .await
            .unwrap();
        tracing::debug!(?model, ?error, ?result, prepared = ?text(declared()), resolved = ?ResolvedAttachment::url(declared().attachments[0].declaration(),SECRET_URL));
        assert_eq!(http.await.unwrap().len(), 1);
    }
    let trace = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(!trace.contains("signature=SECRET-URL"), "locator in trace");
    assert!(!trace.contains("SECRET-DOCUMENT-CONTENT"), "bytes in trace");
    use base64::Engine;
    assert!(
        !trace.contains(&base64::engine::general_purpose::STANDARD.encode(PAYLOAD)),
        "base64 in trace"
    );
}

async fn image_document_wire<T: ModelPort>(make: fn(Config) -> Result<T>, kind: &str) {
    use base64::Engine;
    struct ImageResolver(bool);
    impl AttachmentResolver for ImageResolver {
        fn authorize(
            &self,
            _: &ToolContext,
            _: &str,
            _: &ExternalAttachment,
        ) -> std::result::Result<(), ResolutionError> {
            Ok(())
        }
        fn resolve<'a>(
            &'a self,
            _: &'a ToolContext,
            _: &'a str,
            input: &'a ExternalAttachment,
        ) -> ResolveFuture<'a> {
            Box::pin(async move {
                let png = runtrell_testkit::image_contracts::png();
                Ok(if self.0 {
                    ResolvedAttachment::url(input.clone(), SECRET_URL)
                } else {
                    ResolvedAttachment::bytes(input.clone(), png)
                })
            })
        }
    }
    for url in [false, true] {
        let (endpoint, http) = server(200, "", response(kind, "completed", false)).await;
        let mut c = config(endpoint);
        c.capabilities.documents.image = DocumentTransports {
            url,
            inline_bytes: !url,
        };
        c.attachment_resolver = Some(Arc::new(ImageResolver(url)));
        let mut r = declared();
        let png = runtrell_testkit::image_contracts::png();
        let Attachment::External(input) = &mut r.attachments[0] else {
            unreachable!()
        };
        input.mime_type = "image/png".into();
        input.bytes = png.len() as u64;
        input.digest = content_digest(&png);
        let model = make(c).unwrap();
        model.input_upper_bound(&text(r.clone())).unwrap();
        model.invoke(&context(), text(r)).await.unwrap();
        let sent = http.await.unwrap();
        assert_eq!(sent.len(), 1);
        let body: Value = serde_json::from_str(sent[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let part = if kind == "gemini" {
            &body["contents"][1]["parts"][0]
        } else {
            &body["input"][1]["content"][0]
        };
        match (kind, url) {
            ("gemini", true) => assert_eq!(part["fileData"]["fileUri"], SECRET_URL),
            ("gemini", false) => assert_eq!(
                part["inlineData"]["data"],
                base64::engine::general_purpose::STANDARD.encode(png)
            ),
            (_, true) => assert_eq!(part["image_url"], SECRET_URL),
            (_, false) => assert_eq!(
                part["image_url"],
                format!(
                    "data:image/png;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(png)
                )
            ),
        }
    }
}
#[cfg(feature = "gemini")]
#[tokio::test]
async fn gemini_sensitive_image_wire() {
    image_document_wire(Gemini::new, "gemini").await;
}

#[cfg(feature = "gemini")]
#[tokio::test]
async fn document_http_error_preserves_usage_without_echoing_locator() {
    let (endpoint, http) = server(429,"",json!({"modelVersion":SECRET_URL,"error":{"message":SECRET_URL},"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":1,"thoughtsTokenCount":0}})).await;
    let mut c = config(endpoint);
    c.capabilities.documents.pdf.url = true;
    c.attachment_resolver = Some(Arc::new(Resolver {
        calls: AtomicUsize::new(0),
        url: true,
        mismatch: false,
    }));
    let result = Gemini::new(c)
        .unwrap()
        .invoke(&context(), text(declared()))
        .await
        .unwrap();
    assert!(!format!("{result:?}").contains(SECRET_URL));
    let ExternalResult::Failed { usage, .. } = result else {
        panic!("expected failure")
    };
    assert_eq!(usage.input, Some(3));
    assert_eq!(usage.output, Some(1));
    assert_eq!(http.await.unwrap().len(), 1);
}
#[cfg(feature = "openai")]
#[tokio::test]
async fn openai_sensitive_image_wire() {
    image_document_wire(OpenAi::new, "openai").await;
}
async fn document_wire<T: ModelPort + std::fmt::Debug>(make: fn(Config) -> Result<T>, kind: &str) {
    for url in [false, true] {
        let (endpoint, task) = server(200, "", response(kind, "completed", false)).await;
        let resolver = Arc::new(Resolver {
            calls: AtomicUsize::new(0),
            url,
            mismatch: false,
        });
        let mut c = config(endpoint);
        c.capabilities.documents.pdf = DocumentTransports {
            url: true,
            inline_bytes: true,
        };
        c.attachment_resolver = Some(resolver.clone());
        let model = make(c.clone()).unwrap();
        let original = declared();
        let saved = serde_json::to_string(&original).unwrap();
        let prepared = text(original);
        assert_eq!(model.input_upper_bound(&prepared).unwrap(), 100);
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
        model.invoke(&context(), prepared.clone()).await.unwrap();
        let restored = make(c).unwrap();
        let restored_request = text(serde_json::from_str(&saved).unwrap());
        assert_eq!(
            restored
                .lookup(&context(), restored_request.clone())
                .await
                .unwrap(),
            Lookup::Unknown
        );
        restored.invoke(&context(), restored_request).await.unwrap();
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 2);
        for value in [
            saved,
            format!(
                "{prepared:?} {model:?} {:?}",
                ResolvedAttachment::url(declared().attachments[0].declaration(), SECRET_URL)
            ),
            format!(
                "{:?}",
                ResolvedAttachment::bytes(
                    declared().attachments[0].declaration(),
                    PAYLOAD.to_vec()
                )
            ),
        ] {
            assert!(!value.contains(SECRET_URL));
            assert!(!value.contains("SECRET-DOCUMENT-CONTENT"));
        }
        let sent = task.await.unwrap();
        assert_eq!(sent.len(), 2);
        let wire: Value = serde_json::from_str(sent[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let mut logical_first = wire.clone();
        let mut logical_second: Value =
            serde_json::from_str(sent[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        if url {
            let pointer = if kind == "gemini" {
                "/contents/1/parts/0/fileData/fileUri"
            } else {
                "/input/1/content/0/file_url"
            };
            assert_ne!(
                logical_first.pointer(pointer),
                logical_second.pointer(pointer)
            );
            *logical_first.pointer_mut(pointer).unwrap() = json!(declared().attachments[0]);
            *logical_second.pointer_mut(pointer).unwrap() = json!(declared().attachments[0]);
        }
        assert_eq!(
            logical_first, logical_second,
            "logical input and fixed attachment must match across URL rotation"
        );
        let part = if kind == "gemini" {
            &wire["contents"][1]["parts"][0]
        } else {
            &wire["input"][1]["content"][0]
        };
        if kind == "openai" {
            assert_eq!(part["type"], "input_file");
            if url {
                assert!(part["file_url"].as_str().unwrap().starts_with(SECRET_URL));
            } else {
                use base64::Engine;
                assert_eq!(
                    part["file_data"],
                    format!(
                        "data:application/pdf;base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(PAYLOAD)
                    )
                );
            }
        } else if url {
            assert!(
                part["fileData"]["fileUri"]
                    .as_str()
                    .unwrap()
                    .starts_with(SECRET_URL)
            );
        } else {
            use base64::Engine;
            assert_eq!(
                part["inlineData"]["data"],
                base64::engine::general_purpose::STANDARD.encode(PAYLOAD)
            );
        }
    }
}
#[cfg(feature = "gemini")]
#[tokio::test]
async fn gemini_document_wire_redaction_reconstruction_and_lookup() {
    document_wire(Gemini::new, "gemini").await;
}
#[cfg(feature = "openai")]
#[tokio::test]
async fn openai_document_wire_redaction_reconstruction_and_lookup() {
    document_wire(OpenAi::new, "openai").await;
}

#[cfg(feature = "gemini")]
#[tokio::test]
async fn document_mismatch_capabilities_and_bounds_never_send() {
    let (endpoint, task) = server(200, "", json!({})).await;
    for url in [false, true] {
        let resolver = Arc::new(Resolver {
            calls: AtomicUsize::new(0),
            url,
            mismatch: true,
        });
        let mut c = config(endpoint.clone());
        c.capabilities.documents.pdf = DocumentTransports {
            url: true,
            inline_bytes: true,
        };
        c.attachment_resolver = Some(resolver.clone());
        let model = Gemini::new(c.clone()).unwrap();
        assert!(
            matches!(model.invoke(&context(), text(declared())).await.unwrap(), ExternalResult::Failed { ref code, ref usage, .. } if code == "ATTACHMENT_NOT_SENT" && usage == &Usage { model: "host-model".into(), ..Usage::measured(0,0,0) })
        );
        assert_eq!(
            model.lookup(&context(), text(declared())).await.unwrap(),
            Lookup::Unknown
        );
        let mut bad_digest = declared();
        let Attachment::External(input) = &mut bad_digest.attachments[0] else {
            unreachable!()
        };
        input.bytes = if url { PAYLOAD.len() as u64 + 1 } else { 5 };
        if url {
            input.digest = "0".repeat(64);
        }
        assert!(matches!(
            model.invoke(&context(), text(bad_digest)).await.unwrap(),
            ExternalResult::Failed { .. }
        ));
        let before = resolver.calls.load(Ordering::SeqCst);
        for case in ["capability", "count", "bytes", "wire", "resolver"] {
            let mut invalid = c.clone();
            match case {
                "capability" => invalid.capabilities.documents.pdf = DocumentTransports::default(),
                "count" => invalid.document_limits.max_inputs = 0,
                "bytes" => invalid.document_limits.max_total_bytes = 1,
                "wire" => invalid.max_request_bytes = 100,
                _ => invalid.attachment_resolver = None,
            }
            let rejected = Gemini::new(invalid).unwrap();
            assert!(
                rejected.input_upper_bound(&text(declared())).is_err(),
                "{case}"
            );
            let error = rejected
                .invoke(&context(), text(declared()))
                .await
                .unwrap_err();
            assert!(!format!("{error:?} {error}").contains(SECRET_URL));
        }
        assert_eq!(resolver.calls.load(Ordering::SeqCst), before);
    }
    for url in [false, true] {
        let resolver = Arc::new(Resolver {
            calls: AtomicUsize::new(0),
            url,
            mismatch: false,
        });
        let mut c = config(endpoint.clone());
        c.capabilities.documents.pdf = DocumentTransports {
            url: !url,
            inline_bytes: url,
        };
        c.attachment_resolver = Some(resolver);
        let error = Gemini::new(c)
            .unwrap()
            .invoke(&context(), text(declared()))
            .await
            .unwrap();
        assert!(matches!(error, ExternalResult::Failed { .. }));
    }
    assert!(task.await.unwrap().is_empty());
}

#[cfg(feature = "openrouter")]
#[tokio::test]
async fn chat_documents_unsupported_even_with_host_flags() {
    let (endpoint, task) = server(200, "", json!({})).await;
    let resolver = Arc::new(Resolver {
        calls: AtomicUsize::new(0),
        url: true,
        mismatch: false,
    });
    let mut c = config(endpoint);
    c.capabilities.documents.pdf = DocumentTransports {
        url: true,
        inline_bytes: true,
    };
    c.attachment_resolver = Some(resolver.clone());
    let model = OpenRouter::new(c).unwrap();
    assert!(matches!(
        model.input_upper_bound(&text(declared())),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        model.invoke(&context(), text(declared())).await,
        Err(Error::Unsupported(_))
    ));
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
    assert!(task.await.unwrap().is_empty());
}

#[cfg(feature = "gemini")]
#[tokio::test]
async fn pg_documents_checkpoint_history_and_response_loss_recovery() {
    use base64::Engine;
    use runtrell_postgres::PgStore;
    use runtrell_testkit::{
        BusinessTool, DecisionFinalizer, DecisionValidator, FakePolicy, TestClock,
    };
    let database =
        std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("document Pg integration never skips");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&database)
        .await
        .unwrap();
    for (lost, unsent) in [(false, false), (true, false), (false, true)] {
        for url in [false, true] {
            let tick = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap();
            let schema = format!("http_docs_{}", tick.as_nanos());
            let store = Arc::new(
                PgStore::open(pool.clone(), &schema, Capacity::default())
                    .await
                    .unwrap(),
            );
            let account = store
                .create_account("tenant", Amount::new(10000, 10000), None)
                .await
                .unwrap();
            let handle = store
                .start(RunSpec::new(
                    "tenant",
                    "extract",
                    account.clone(),
                    "extract document",
                ))
                .await
                .unwrap();
            let claim = store
                .claim(&handle, "worker", tick.as_secs(), 100)
                .await
                .unwrap();
            let mut body = response("gemini", "completed", false);
            body["modelVersion"] = json!("host-model");
            body["candidates"][0]["content"]["parts"] =
                json!([{"text":"{\"decision\":\"answered\"}"}]);
            body["usageMetadata"] =
                json!({"promptTokenCount":3,"candidatesTokenCount":1,"thoughtsTokenCount":0});
            let (endpoint, http) = if lost {
                raw_server(String::new()).await
            } else {
                server(200, "", body).await
            };
            let resolver = Arc::new(Resolver {
                calls: AtomicUsize::new(0),
                url,
                mismatch: unsent,
            });
            let mut c = config(endpoint);
            c.run = handle.clone();
            c.capabilities.documents.pdf = DocumentTransports {
                url: true,
                inline_bytes: true,
            };
            c.attachment_resolver = Some(resolver.clone());
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
            let clock = TestClock::default();
            clock.set(tick.as_secs());
            let make_runtime = |store| Runtime {
                store,
                model: DocumentModel(Gemini::new(c.clone()).unwrap()),
                tools: Vec::<BusinessTool>::new(),
                policy: FakePolicy::default(),
                clock: clock.clone(),
                validator: Arc::new(DecisionValidator::default()),
                finalizer: Arc::new(DecisionFinalizer),
            };
            let runtime = make_runtime(store.clone());
            let run = runtime.drive(&claim).await.unwrap();
            let ops = store.list_operations(&handle).await.unwrap();
            assert_eq!(ops.len(), 1);
            assert_eq!(
                ops[0].request.input["attachments"],
                json!(declared().attachments)
            );
            assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
            if lost {
                assert!(matches!(run.state, RunState::Waiting(_)));
                let before = store.account(&account).await.unwrap();
                assert_eq!(before.unknown, 1);
                assert!(!ops[0].settled);
                drop(runtime);
                drop(store);
                let independent = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(4)
                    .connect(&database)
                    .await
                    .unwrap();
                let reopened = Arc::new(
                    PgStore::open(independent, &schema, Capacity::default())
                        .await
                        .unwrap(),
                );
                clock.set(tick.as_secs() + 101);
                let next = reopened
                    .claim(&handle, "reopened", tick.as_secs() + 101, 100)
                    .await
                    .unwrap();
                let run = make_runtime(reopened.clone()).drive(&next).await.unwrap();
                assert!(matches!(
                    run.state,
                    RunState::Terminal(Terminal::OutcomeUnknown(_))
                ));
                assert!(
                    resolver.calls.load(Ordering::SeqCst) == 1,
                    "lookup must not resolve again"
                );
                assert_eq!(
                    reopened.account(&account).await.unwrap().reserved,
                    before.reserved
                );
                let op = &reopened.list_operations(&handle).await.unwrap()[0];
                assert_eq!(op.send_count, 1);
                assert!(!op.settled);
            } else if unsent {
                assert!(matches!(run.state, RunState::Terminal(Terminal::Failed(_))));
                assert!(ops[0].settled);
                let account = store.account(&account).await.unwrap();
                assert_eq!(account.used, Amount::default());
                assert_eq!(account.reserved, Amount::default());
                assert_eq!(account.unknown, 0);
            } else {
                assert_eq!(run.state, RunState::Terminal(Terminal::Succeeded));
                assert!(run.checkpoint.is_some());
                assert!(ops[0].settled);
                assert_eq!(
                    store.account(&account).await.unwrap().used,
                    Amount::new(4, 4)
                );
                assert_eq!(
                    runtime.drive(&claim).await.unwrap().state,
                    RunState::Terminal(Terminal::Succeeded)
                );
                assert_eq!(
                    resolver.calls.load(Ordering::SeqCst),
                    1,
                    "saved result must not acquire originals"
                );
            }
            let requests = http.await.unwrap();
            assert_eq!(
                requests.len(),
                usize::from(!unsent),
                "only sent inference may reach HTTP"
            );
            if !unsent {
                assert!(requests[0].contains(if url { SECRET_URL } else { "JVBERi" }));
            }
            let tables: Vec<String> =
                sqlx::query_scalar("SELECT tablename FROM pg_tables WHERE schemaname=$1")
                    .bind(&schema)
                    .fetch_all(&pool)
                    .await
                    .unwrap();
            for table in tables {
                let rows: Vec<String> = sqlx::query_scalar(&format!(
                    "SELECT row_to_json(t)::text FROM {schema}.{table} t"
                ))
                .fetch_all(&pool)
                .await
                .unwrap();
                for row in rows {
                    assert!(!row.contains(SECRET_URL), "locator in {table}");
                    assert!(!row.contains("SECRET-DOCUMENT-CONTENT"), "bytes in {table}");
                    assert!(
                        !row.contains(&base64::engine::general_purpose::STANDARD.encode(PAYLOAD)),
                        "base64 in {table}"
                    );
                }
            }
        }
    }
}

#[cfg(feature = "gemini")]
fn pdf() -> Vec<u8> {
    use lopdf::{Document, Object, Stream, dictionary};
    let mut doc = Document::with_version("1.5");
    let pages = doc.new_object_id();
    let mut kids = Vec::new();
    for text in ["FIRST_PAGE_ONLY", "SECOND_PAGE_ONLY", "THIRD_PAGE_ONLY"] {
        let content = doc.add_object(Stream::new(
            dictionary! {},
            format!("BT ({text}) Tj ET").into_bytes(),
        ));
        let page = doc.add_object(dictionary! {"Type" => "Page", "Parent" => pages, "Contents" => content, "MediaBox" => vec![0.into(),0.into(),100.into(),100.into()]});
        kids.push(Object::Reference(page));
    }
    doc.objects.insert(
        pages,
        Object::Dictionary(dictionary! {"Type" => "Pages", "Kids" => kids, "Count" => 3}),
    );
    let catalog = doc.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages});
    doc.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

#[cfg(feature = "gemini")]
pub(super) async fn sequence_server(
    bodies: Vec<Value>,
) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/test", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut captures = Vec::new();
        for body in bodies {
            let (mut stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut data = Vec::new();
            let end = loop {
                let mut buf = [0; 4096];
                let n = stream.read(&mut buf).await.unwrap();
                assert_ne!(n, 0);
                data.extend_from_slice(&buf[..n]);
                if let Some(end) = data.windows(4).position(|s| s == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let header = String::from_utf8_lossy(&data[..end]);
            let length: usize = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_owned)
                })
                .unwrap()
                .parse()
                .unwrap();
            while data.len() < end + length {
                let mut buf = [0; 4096];
                let n = stream.read(&mut buf).await.unwrap();
                assert_ne!(n, 0);
                data.extend_from_slice(&buf[..n]);
            }
            captures.push(serde_json::from_slice(&data[end..end + length]).unwrap());
            let text = body.to_string();
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",text.len()).as_bytes()).await.unwrap();
        }
        captures
    });
    (endpoint, task)
}

#[cfg(feature = "gemini")]
#[tokio::test]
async fn pg_document_view_selects_pages_and_attaches_fixed_workspace_and_artifact_versions() {
    use base64::Engine;
    use runtrell::{data::*, document_view::*, images::*, standard_tools::*};
    use runtrell_postgres::PgStore;
    use runtrell_testkit::{DecisionFinalizer, DecisionValidator, FakePolicy, TestClock};
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("Pg tests never skip");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    struct PdfResolver {
        calls: AtomicUsize,
    }
    impl AttachmentResolver for PdfResolver {
        fn authorize(
            &self,
            _: &ToolContext,
            _: &str,
            _: &ExternalAttachment,
        ) -> std::result::Result<(), ResolutionError> {
            Ok(())
        }
        fn resolve<'a>(
            &'a self,
            _: &'a ToolContext,
            _: &'a str,
            input: &'a ExternalAttachment,
        ) -> ResolveFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(ResolvedAttachment::bytes(input.clone(), pdf()))
            })
        }
    }
    for source in ["workspace", "artifact", "external"] {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let schema = format!("document_view_{}", uuid_id());
        let store = Arc::new(
            PgStore::open(pool.clone(), &schema, Capacity::default())
                .await
                .unwrap(),
        );
        let account = store
            .create_account("tenant", Amount::new(10000, 10000), None)
            .await
            .unwrap();
        let handle = store
            .start(RunSpec::new(
                "tenant",
                "document-view",
                account.clone(),
                "Read page two",
            ))
            .await
            .unwrap();
        let claim = store.claim(&handle, "worker", now, 100).await.unwrap();
        let access = Access::owner(Namespace {
            owner: handle.clone(),
            agent: "main".into(),
        });
        let data = DataStore::memory(StoragePolicy::default());
        let bytes = pdf();
        let reference = if source == "artifact" {
            let staged = data
                .stage_named(
                    &access,
                    ArtifactSpec {
                        id: "source".into(),
                        media_type: "application/pdf".into(),
                        class: DataClass::Intermediate,
                    },
                    &bytes,
                    0,
                )
                .unwrap();
            ImageReference::Artifact(data.save(&access, staged, 0).await.unwrap())
        } else {
            let snapshot = data
                .write(&access, "source.pdf", None, &bytes)
                .await
                .unwrap();
            ImageReference::Workspace {
                namespace: access.namespace().clone(),
                file: snapshot,
                mime_type: "application/pdf".into(),
            }
        };
        let attachment = if source == "external" {
            Attachment::External(ExternalAttachment {
                reference_id: "external-pdf".into(),
                version: "v1".into(),
                pages: None,
                mime_type: "application/pdf".into(),
                bytes: bytes.len() as u64,
                digest: content_digest(&bytes),
            })
        } else {
            Attachment::Stored(reference)
        };
        let input = json!({"attachment":attachment,"pages":{"first":2,"last":2}});
        let external = Arc::new(PdfResolver {
            calls: AtomicUsize::new(0),
        });
        let make_resolver = || ImageResolver {
            store: data.clone(),
            policy: FakePolicy::default(),
            accesses: vec![access.clone()],
            limits: ImageLimits::default(),
        };
        let clock = TestClock::default();
        clock.set(now);
        let tool = || DocumentView {
            resolver: make_resolver(),
            external: Some(external.clone()),
            runs: store.clone(),
            clock: clock.clone(),
            output_access: access.clone(),
            limits: DocumentViewLimits::default(),
        };
        let mut first = response("gemini", "completed", false);
        first["usageMetadata"] =
            json!({"promptTokenCount":3,"candidatesTokenCount":1,"thoughtsTokenCount":0});
        first["candidates"][0]["content"]["parts"] =
            json!([{"functionCall":{"id":"view","name":"document.view","args":input}}]);
        let mut last = response("gemini", "completed", false);
        last["usageMetadata"] = first["usageMetadata"].clone();
        last["candidates"][0]["content"]["parts"] = json!([{"text":"{\"decision\":\"answered\"}"}]);
        let (endpoint, http) = sequence_server(vec![first, last]).await;
        let mut c = config(endpoint);
        c.run = handle.clone();
        c.capabilities.limits.timeout = 60;
        c.capabilities.tool_calling = true;
        c.capabilities.documents.pdf.inline_bytes = true;
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
        let runtime = Runtime {
            store: store.clone(),
            model: ImageModel {
                inner: Gemini::new(c).unwrap(),
                resolver: make_resolver(),
                runs: store.clone(),
            },
            tools: ToolSet::default()
                .document_view(tool())
                .select(StandardTool::DocumentView)
                .build()
                .unwrap(),
            policy: FakePolicy::default(),
            clock: clock.clone(),
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let run = runtime.drive(&claim).await.unwrap();
        assert_eq!(
            run.state,
            RunState::Terminal(Terminal::Succeeded),
            "operations: {:?}",
            store.list_operations(&handle).await.unwrap()
        );
        let sent = http.await.unwrap();
        assert_eq!(sent.len(), 2);
        let parts = sent[1]["contents"].as_array().unwrap();
        let encoded = parts
            .iter()
            .flat_map(|p| p["parts"].as_array().unwrap())
            .find_map(|p| p["inlineData"]["data"].as_str())
            .unwrap();
        let selected = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert!(!String::from_utf8_lossy(&selected).contains("FIRST_PAGE_ONLY"));
        assert!(!String::from_utf8_lossy(&selected).contains("THIRD_PAGE_ONLY"));
        let selected = lopdf::Document::load_mem(&selected).unwrap();
        assert_eq!(selected.get_pages().len(), 1);
        let content = selected
            .get_page_content(*selected.get_pages().values().next().unwrap())
            .unwrap();
        assert!(String::from_utf8_lossy(&content).contains("SECOND_PAGE_ONLY"));
        assert!(!String::from_utf8_lossy(&content).contains("FIRST_PAGE_ONLY"));
        let ops = store.list_operations(&handle).await.unwrap();
        let op = ops
            .iter()
            .find(|o| o.request.target == "document.view")
            .unwrap();
        let ctx = ToolContext {
            claim: claim.clone(),
            principal: "worker".into(),
            operation: op.id.clone(),
            attempt: op.request.attempt,
            deadline: None,
            permission: "document.view".into(),
            account_binding: "documents".into(),
            recovery_proof: None,
        };
        // A saved result needs no original: remove its source, then recover the receipt.
        if source == "workspace" {
            let current = data.snapshot(&access).await.unwrap();
            data.delete(&access, "source.pdf", current.revision)
                .await
                .unwrap();
        }
        assert!(matches!(
            tool().lookup(&ctx, input.clone()).await.unwrap(),
            Lookup::Found(_)
        ));
        assert_eq!(
            external.calls.load(Ordering::SeqCst),
            usize::from(source == "external")
        );
        if source != "workspace" {
            let inspect = json!({"attachment":attachment,"pages":null});
            let mut inspect_context = ctx.clone();
            inspect_context.operation = "inspect-pages".into();
            let ExternalResult::Applied { output, .. } =
                tool().invoke(&inspect_context, inspect).await.unwrap()
            else {
                panic!("page count")
            };
            assert_eq!(output["page_count"], 3);
            assert!(output.get("attachments").is_none());
        }
        let invalid = json!({"attachment":Attachment::Stored(ImageReference::Workspace { namespace:access.namespace().clone(),file:FileVersion {path:"bad.txt".into(),revision:1,hash:content_digest(b"bad"),bytes:3},mime_type:"text/plain".into() }),"pages":null});
        let png = runtrell_testkit::image_contracts::png();
        let staged = data
            .stage_named(
                &access,
                ArtifactSpec {
                    id: "input-png".into(),
                    media_type: "image/png".into(),
                    class: DataClass::Intermediate,
                },
                &png,
                now,
            )
            .unwrap();
        let image = data.save(&access, staged, now).await.unwrap();
        let mut image_context = ctx.clone();
        image_context.operation = "view-image".into();
        let ExternalResult::Applied { output,.. } = tool().invoke(&image_context,json!({"attachment":Attachment::Stored(ImageReference::Artifact(image)),"pages":{"first":1,"last":1}})).await.unwrap() else { panic!("PNG selection") };
        assert_eq!(output["page_count"], 1);
        assert_eq!(output["image_inputs"].as_array().unwrap().len(), 1);
        assert!(matches!(
            tool()
                .quote(&store.get(&handle).await.unwrap(), &invalid)
                .await,
            Err(Error::Unsupported(_))
        ));
        let denied = tool();
        denied.resolver.policy.revoked.store(true, Ordering::SeqCst);
        let calls = external.calls.load(Ordering::SeqCst);
        assert_eq!(
            denied.lookup(&ctx, input.clone()).await.unwrap_err(),
            Error::PermissionDenied
        );
        assert_eq!(external.calls.load(Ordering::SeqCst), calls);
    }
}
#[cfg(feature = "gemini")]
fn uuid_id() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_string()
}
