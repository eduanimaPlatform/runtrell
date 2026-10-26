// SPDX-License-Identifier: Apache-2.0
use super::*;
use runtrell::data::content_digest;
use runtrell::sensitive_inputs::*;
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
        if request.sensitive_inputs.is_empty() {
            request.sensitive_inputs = declared().sensitive_inputs;
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
impl SensitiveInputResolver for Resolver {
    fn resolve<'a>(
        &'a self,
        _: &'a ToolContext,
        _: &'a str,
        _: &'a SensitiveInput,
    ) -> ResolveFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.url {
                Ok(ResolvedSensitiveInput::url(
                    SECRET_URL,
                    PAYLOAD.len() as u64 + u64::from(self.mismatch),
                    content_digest(PAYLOAD),
                ))
            } else {
                Ok(ResolvedSensitiveInput::bytes(if self.mismatch {
                    b"wrong".to_vec()
                } else {
                    PAYLOAD.to_vec()
                }))
            }
        })
    }
}
fn declared() -> ModelRequest {
    let mut r = request();
    r.sensitive_inputs = vec![SensitiveInput {
        reference_id: "opaque-fixed-version".into(),
        mime_type: "application/pdf".into(),
        bytes: PAYLOAD.len() as u64,
        digest: content_digest(PAYLOAD),
    }];
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
        c.sensitive_input_resolver = Some(Arc::new(Resolver {
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
        impl SensitiveInputResolver for Refuse {
            fn resolve<'a>(
                &'a self,
                _: &'a ToolContext,
                _: &'a str,
                _: &'a SensitiveInput,
            ) -> ResolveFuture<'a> {
                Box::pin(async { Err(ResolutionError) })
            }
        }
        c.sensitive_input_resolver = Some(Arc::new(Refuse));
        let error = Gemini::new(c)
            .unwrap()
            .invoke(&context(), text(declared()))
            .await
            .unwrap_err();
        tracing::debug!(?model, ?error, ?result, prepared = ?text(declared()), resolved = ?ResolvedSensitiveInput::url(SECRET_URL,1,SECRET_URL));
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
    impl SensitiveInputResolver for ImageResolver {
        fn resolve<'a>(
            &'a self,
            _: &'a ToolContext,
            _: &'a str,
            _: &'a SensitiveInput,
        ) -> ResolveFuture<'a> {
            Box::pin(async move {
                let png = runtrell_testkit::image_contracts::png();
                Ok(if self.0 {
                    ResolvedSensitiveInput::url(SECRET_URL, png.len() as u64, content_digest(&png))
                } else {
                    ResolvedSensitiveInput::bytes(png)
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
        c.sensitive_input_resolver = Some(Arc::new(ImageResolver(url)));
        let mut r = declared();
        let png = runtrell_testkit::image_contracts::png();
        r.sensitive_inputs[0].mime_type = "image/png".into();
        r.sensitive_inputs[0].bytes = png.len() as u64;
        r.sensitive_inputs[0].digest = content_digest(&png);
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
    c.sensitive_input_resolver = Some(Arc::new(Resolver {
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
        c.sensitive_input_resolver = Some(resolver.clone());
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
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 3);
        for value in [
            saved,
            format!(
                "{prepared:?} {model:?} {:?}",
                ResolvedSensitiveInput::url(
                    SECRET_URL,
                    PAYLOAD.len() as u64,
                    content_digest(PAYLOAD)
                )
            ),
            format!("{:?}", ResolvedSensitiveInput::bytes(PAYLOAD.to_vec())),
        ] {
            assert!(!value.contains(SECRET_URL));
            assert!(!value.contains("SECRET-DOCUMENT-CONTENT"));
        }
        let sent = task.await.unwrap();
        assert_eq!(sent.len(), 2);
        let wire: Value = serde_json::from_str(sent[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let part = if kind == "gemini" {
            &wire["contents"][1]["parts"][0]
        } else {
            &wire["input"][1]["content"][0]
        };
        if kind == "openai" {
            assert_eq!(part["type"], "input_file");
            if url {
                assert_eq!(part["file_url"], SECRET_URL);
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
            assert_eq!(part["fileData"]["fileUri"], SECRET_URL);
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
        c.sensitive_input_resolver = Some(resolver.clone());
        let model = Gemini::new(c.clone()).unwrap();
        assert_eq!(
            model
                .invoke(&context(), text(declared()))
                .await
                .unwrap_err(),
            Error::Conflict
        );
        assert_eq!(
            model
                .lookup(&context(), text(declared()))
                .await
                .unwrap_err(),
            Error::Conflict
        );
        let mut bad_digest = declared();
        bad_digest.sensitive_inputs[0].bytes = if url { PAYLOAD.len() as u64 + 1 } else { 5 };
        if url {
            bad_digest.sensitive_inputs[0].digest = "0".repeat(64);
        }
        assert_eq!(
            model
                .invoke(&context(), text(bad_digest))
                .await
                .unwrap_err(),
            Error::Conflict
        );
        let before = resolver.calls.load(Ordering::SeqCst);
        for case in ["capability", "count", "bytes", "wire", "resolver"] {
            let mut invalid = c.clone();
            match case {
                "capability" => invalid.capabilities.documents.pdf = DocumentTransports::default(),
                "count" => invalid.document_limits.max_inputs = 0,
                "bytes" => invalid.document_limits.max_total_bytes = 1,
                "wire" => invalid.max_request_bytes = 100,
                _ => invalid.sensitive_input_resolver = None,
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
        c.sensitive_input_resolver = Some(resolver);
        let error = Gemini::new(c)
            .unwrap()
            .invoke(&context(), text(declared()))
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Unsupported(_)));
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
    c.sensitive_input_resolver = Some(resolver.clone());
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
    for lost in [false, true] {
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
                mismatch: false,
            });
            let mut c = config(endpoint);
            c.run = handle.clone();
            c.capabilities.documents.pdf = DocumentTransports {
                url: true,
                inline_bytes: true,
            };
            c.sensitive_input_resolver = Some(resolver.clone());
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
                ops[0].request.input["sensitive_inputs"],
                json!(declared().sensitive_inputs)
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
                    resolver.calls.load(Ordering::SeqCst) > 1,
                    "lookup must resolve again"
                );
                assert_eq!(
                    reopened.account(&account).await.unwrap().reserved,
                    before.reserved
                );
                let op = &reopened.list_operations(&handle).await.unwrap()[0];
                assert_eq!(op.send_count, 1);
                assert!(!op.settled);
            } else {
                assert_eq!(run.state, RunState::Terminal(Terminal::Succeeded));
                assert!(run.checkpoint.is_some());
                assert!(ops[0].settled);
                assert_eq!(
                    store.account(&account).await.unwrap().used,
                    Amount::new(4, 4)
                );
            }
            let requests = http.await.unwrap();
            assert_eq!(requests.len(), 1, "recovery must not resend");
            assert!(requests[0].contains(if url { SECRET_URL } else { "JVBERi" }));
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
