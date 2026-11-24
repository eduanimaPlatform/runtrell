// SPDX-License-Identifier: Apache-2.0
use super::*;
use runtrell_testkit::{
    BusinessTool, DecisionFinalizer, DecisionValidator, FakeModel, FakePolicy, TestClock,
    outcome_contracts::{
        OUTCOME_CASES, OutcomeCase, OutcomeException, assert_no_unexpected_ambiguity,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wire {
    Responses,
    Chat,
    GenerateContent,
    Images,
    ImageEdit,
    GenerateImages,
    GenerateImageEdit,
}

impl Wire {
    fn unproven_rejection_usage(self, case: OutcomeCase) -> bool {
        match case {
            OutcomeCase::Rejection(status) => {
                (self == Self::Responses && status == 429)
                    || (self.gemini() && matches!(status, 402 | 413 | 422))
            }
            OutcomeCase::ErrorEnvelope => matches!(self, Self::Responses | Self::Chat),
            _ => false,
        }
    }

    fn image(self) -> bool {
        matches!(
            self,
            Self::Images | Self::ImageEdit | Self::GenerateImages | Self::GenerateImageEdit
        )
    }

    fn gemini(self) -> bool {
        matches!(
            self,
            Self::GenerateContent | Self::GenerateImages | Self::GenerateImageEdit
        )
    }

    fn rejection(self, status: u16) -> Value {
        let error = if self.gemini() {
            let tag = match status {
                401 => "UNAUTHENTICATED",
                403 => "PERMISSION_DENIED",
                404 => "NOT_FOUND",
                429 => "RESOURCE_EXHAUSTED",
                _ => "INVALID_ARGUMENT",
            };
            json!({"code":status,"status":tag,"message":"fixture"})
        } else if self == Self::Chat {
            json!({"code":status,"message":"fixture"})
        } else {
            json!({"type":"invalid_request_error","code":"invalid_parameter","message":"fixture"})
        };
        json!({"error":error})
    }

    fn usage(self, body: &mut Value, case: OutcomeCase) {
        let output = match case {
            OutcomeCase::FailureWithPartialUsage => Value::Null,
            OutcomeCase::FailureWithInvalidUsage | OutcomeCase::InvalidSuccessUsage => {
                json!("invalid")
            }
            _ => json!(5),
        };
        if self.gemini() {
            body["usageMetadata"] = json!({"promptTokenCount":3,"candidatesTokenCount":output,
                "thoughtsTokenCount":0});
        } else if self == Self::Chat {
            body["usage"] = json!({"prompt_tokens":3,"completion_tokens":output});
        } else {
            body["usage"] = json!({"input_tokens":3,"output_tokens":output,
                "input_tokens_details":{"text_tokens":3,"image_tokens":0,"cached_tokens":0},
                "output_tokens_details":{"image_tokens":5}});
        }
    }

    fn raw_reply(self, case: OutcomeCase) -> String {
        let (status, body) = match case {
            OutcomeCase::Disconnect => return String::new(),
            OutcomeCase::TruncatedRejection => return raw(401, "{", 99),
            OutcomeCase::TruncatedFailure => return raw(500, "{", 99),
            OutcomeCase::TruncatedSuccess => return raw(200, "{", 99),
            OutcomeCase::InvalidJson => return raw(200, "not json", 8),
            OutcomeCase::ResponseByteLimit | OutcomeCase::OversizedRejection => {
                let large = "x".repeat(4096);
                let status = if matches!(case, OutcomeCase::OversizedRejection) {
                    403
                } else {
                    200
                };
                return raw(status, &large, large.len());
            }
            OutcomeCase::Rejection(status) => (status, self.rejection(status)),
            OutcomeCase::ErrorEnvelope => (200, self.rejection(400)),
            OutcomeCase::InvalidToolInput => {
                let mut body = match self {
                    Self::Responses => {
                        let mut reply = response("openai", "tool_call", true);
                        reply["output"][0]["arguments"] = "{".into();
                        reply
                    }
                    Self::Chat => {
                        let mut reply = response("openrouter", "tool_call", true);
                        reply["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] =
                            "{".into();
                        reply
                    }
                    Self::GenerateContent => {
                        let mut reply = response("gemini", "tool_call", true);
                        reply["candidates"][0]["content"]["parts"][0]["functionCall"]["args"] =
                            Value::Null;
                        reply
                    }
                    _ => unreachable!("image tool validates local arguments"),
                };
                self.usage(&mut body, case);
                (200, body)
            }
            OutcomeCase::InvalidSchema => {
                let mut body = json!({});
                self.usage(&mut body, case);
                (200, body)
            }
            OutcomeCase::InvalidSuccessUsage => {
                let mut body = match self {
                    Self::Responses => response("openai", "completed", false),
                    Self::Chat => response("openrouter", "stop", false),
                    Self::GenerateContent => response("gemini", "STOP", false),
                    Self::Images | Self::ImageEdit => {
                        json!({"data":[{"b64_json":base64::Engine::encode(
                            &base64::engine::general_purpose::STANDARD, runtrell_testkit::image_contracts::png()
                        )}]})
                    }
                    Self::GenerateImages | Self::GenerateImageEdit => {
                        json!({"candidates":[{"finishReason":"STOP","content":{"parts":[
                            {"inlineData":{"mimeType":"image/png","data":base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD, runtrell_testkit::image_contracts::png()
                            )}}
                        ]}}]})
                    }
                };
                self.usage(&mut body, case);
                (200, body)
            }
            case => {
                let mut body = self.rejection(500);
                if !matches!(case, OutcomeCase::FailureWithoutUsage) {
                    self.usage(&mut body, case);
                }
                (500, body)
            }
        };
        let body = body.to_string();
        raw(status, &body, body.len())
    }
}

fn raw(status: u16, body: &str, length: usize) -> String {
    format!(
        "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{body}"
    )
}

async fn model_invoke<M: ModelPort>(
    make: impl FnOnce(Config) -> Result<M>,
    config: Config,
    context: ToolContext,
    input: Value,
    case: OutcomeCase,
) -> Result<ExternalResult> {
    let mut request = if matches!(case, OutcomeCase::InvalidToolInput) {
        tool_request()
    } else {
        request()
    };
    if let Some(prompt) = input["prompt"].as_str() {
        request.messages = vec![Message::text("user", prompt)];
    }
    make(config)?.invoke(&context, text(request)).await
}

async fn invoke<S: ControlStore>(
    store: Arc<S>,
    wire: Wire,
    config: Config,
    context: ToolContext,
    input: Value,
    case: OutcomeCase,
) -> Result<ExternalResult> {
    // Each concrete adapter is also exercised by the feature CI jobs.
    match wire {
        #[cfg(feature = "openai")]
        Wire::Responses => {
            model_invoke(
                |config| {
                    let mut model = OpenAi::new(config)?;
                    if matches!(case, OutcomeCase::Builder) {
                        model.0.config.endpoint = "not a URL".into();
                    }
                    Ok(model)
                },
                config,
                context,
                input,
                case,
            )
            .await
        }
        #[cfg(feature = "openrouter")]
        Wire::Chat => {
            model_invoke(
                |config| {
                    let mut model = OpenRouter::new(config)?;
                    if matches!(case, OutcomeCase::Builder) {
                        model.0.config.endpoint = "not a URL".into();
                    }
                    Ok(model)
                },
                config,
                context,
                input,
                case,
            )
            .await
        }
        #[cfg(feature = "gemini")]
        Wire::GenerateContent => {
            model_invoke(
                |config| {
                    let mut model = Gemini::new(config)?;
                    if matches!(case, OutcomeCase::Builder) {
                        model.0.config.endpoint = "not a URL".into();
                    }
                    Ok(model)
                },
                config,
                context,
                input,
                case,
            )
            .await
        }
        #[cfg(feature = "openai")]
        Wire::Images | Wire::ImageEdit => {
            openai_image_contracts::invoke_outcome_case(
                store,
                context,
                config,
                input,
                case,
                matches!(wire, Wire::ImageEdit | Wire::GenerateImageEdit),
            )
            .await
        }
        #[cfg(feature = "gemini")]
        Wire::GenerateImages | Wire::GenerateImageEdit => {
            image_contracts::invoke_outcome_case(
                store,
                context,
                config,
                input,
                case,
                matches!(wire, Wire::ImageEdit | Wire::GenerateImageEdit),
            )
            .await
        }
        #[allow(unreachable_patterns)]
        _ => {
            drop((store, config, context, input));
            let _ = case;
            unreachable!("feature is disabled")
        }
    }
}

fn ambiguity_reason(case: OutcomeCase) -> Option<AmbiguityReason> {
    match case {
        OutcomeCase::Disconnect => Some(AmbiguityReason::TransportAfterPossibleWrite),
        OutcomeCase::TruncatedSuccess => Some(AmbiguityReason::ResponseLost),
        OutcomeCase::InvalidJson => Some(AmbiguityReason::ResponseDecode),
        OutcomeCase::ResponseByteLimit => Some(AmbiguityReason::ResponseByteLimit),
        _ => None,
    }
}

fn assert_receipt(case: OutcomeCase, wire: Wire, receipt: &ExternalResult) {
    let ambiguous = ambiguity_reason(case);
    if let Some(reason) = ambiguous {
        assert_eq!(
            receipt.ambiguity().map(AmbiguousOutcome::reason),
            Some(reason),
            "{wire:?} {case:?}"
        );
        return;
    }
    if !wire.image() && matches!(case, OutcomeCase::InvalidToolInput) {
        let ExternalResult::Applied { output, usage, .. } = receipt else {
            panic!("the model completed with a malformed tool call");
        };
        let reply: ModelReply = serde_json::from_value(output.clone()).unwrap();
        assert_eq!(reply.calls.len(), 1);
        assert_eq!(reply.calls[0].arguments, Value::Null);
        assert_eq!(usage.amount(), Ok(Amount::new(8, 8)));
        return;
    }
    if !wire.image() && matches!(case, OutcomeCase::InvalidSchema) {
        assert!(
            matches!(
                receipt,
                ExternalResult::Failed {
                    code,
                    retry_after: Some(0),
                    ..
                } if code == "PROVIDER_RESPONSE_INVALID"
            ),
            "{wire:?} {case:?}: {receipt:?}"
        );
    } else if matches!(case, OutcomeCase::InvalidSuccessUsage) {
        assert!(
            matches!(receipt, ExternalResult::Applied { .. }),
            "{wire:?} {receipt:?}"
        );
    } else {
        assert!(
            matches!(
                receipt,
                ExternalResult::Failed {
                    retry_after: None,
                    ..
                }
            ),
            "{wire:?} {case:?}: {receipt:?}"
        );
    }
    match case {
        OutcomeCase::Dns
        | OutcomeCase::Connect
        | OutcomeCase::Tls
        | OutcomeCase::DeadlineBeforePoll => {
            assert_eq!(
                receipt.usage_outcome(),
                UsageOutcome::NonBillable(NonBillableReason::RetryableNotSent)
            );
        }
        OutcomeCase::Builder
        | OutcomeCase::RequestByteLimit
        | OutcomeCase::PermissionDenied
        | OutcomeCase::InvalidToolInput
        | OutcomeCase::OversizedRejection => {
            assert!(
                matches!(receipt.usage_outcome(), UsageOutcome::NonBillable(_)),
                "{case:?} {receipt:?}"
            );
        }
        OutcomeCase::TruncatedRejection => {
            let ExternalResult::Failed {
                code,
                repair_hint: None,
                ..
            } = receipt
            else {
                panic!("expected an operator-facing authentication failure: {receipt:?}");
            };
            let failure: Failure = serde_json::from_str(code).unwrap();
            assert_eq!(failure.class, ErrorClass::Authentication);
            assert_eq!(failure.status, 401);
            assert_eq!(
                receipt.usage_outcome(),
                UsageOutcome::NonBillable(NonBillableReason::RejectedBeforeExecution)
            );
            let diagnostic = receipt.provider_diagnostic().unwrap();
            assert_eq!(diagnostic.http_status, Some(401));
            assert_eq!(diagnostic.phase, ProviderFailurePhase::ResponseRead);
        }
        OutcomeCase::Rejection(_) | OutcomeCase::ErrorEnvelope => {
            assert_eq!(
                matches!(receipt.usage_outcome(), UsageOutcome::NonBillable(_)),
                !wire.unproven_rejection_usage(case),
            );
        }
        OutcomeCase::FailureWithUsage | OutcomeCase::InvalidSchema => {
            let UsageOutcome::Measured(usage) = receipt.usage_outcome() else {
                panic!("{wire:?} {case:?} should retain usage: {receipt:?}");
            };
            assert_eq!(usage.amount(), Ok(Amount::new(8, 8)));
        }
        _ => assert!(
            matches!(receipt.usage_outcome(), UsageOutcome::Unresolved { .. }),
            "{case:?} {receipt:?}"
        ),
    }
}

async fn contract<S: ControlStore>(store: Arc<S>, wire: Wire, kind: OperationKind) {
    // Parallel wires exercise independent callers. Share the store, but avoid
    // racing to provision the same tenant's capacity rows.
    let tenant = format!("fixture-{wire:?}-{kind:?}").to_ascii_lowercase();
    for &case in OUTCOME_CASES {
        let mut spy = None;
        let (endpoint, server) = match case {
            OutcomeCase::Dns => ("https://provider-fixture.invalid/test".to_owned(), None),
            OutcomeCase::Connect => {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                (
                    format!("http://{}/test", listener.local_addr().unwrap()),
                    None,
                )
            }
            OutcomeCase::Tls => {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let endpoint = format!("https://{}/test", listener.local_addr().unwrap());
                let server = tokio::spawn(async move {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut hello = [0; 2048];
                    assert!(stream.read(&mut hello).await.unwrap() > 0);
                    stream.write_all(b"invalid protocol").await.unwrap();
                    vec![]
                });
                (endpoint, Some(server))
            }
            OutcomeCase::InvalidToolInput if !wire.image() => {
                let (endpoint, server) = raw_server(wire.raw_reply(case)).await;
                (endpoint, Some(server))
            }
            OutcomeCase::Builder
            | OutcomeCase::RequestByteLimit
            | OutcomeCase::DeadlineBeforePoll
            | OutcomeCase::InvalidToolInput
            | OutcomeCase::PermissionDenied => {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let endpoint = format!("http://{}/test", listener.local_addr().unwrap());
                spy = Some(listener);
                (endpoint, None)
            }
            case => {
                let (endpoint, server) = raw_server(wire.raw_reply(case)).await;
                (endpoint, Some(server))
            }
        };
        let mut config = config(endpoint);
        config.capabilities.counters.reasoning = false;
        config.capabilities.limits.price = Some(Price {
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
        clock.set(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        );
        let now = clock.now();
        let account = store
            .create_account(&tenant, Amount::new(100_000, 100_000), None)
            .await
            .unwrap();
        let key = format!("fixture-{wire:?}-{kind:?}-{case:?}");
        let run = store
            .start(RunSpec::new(&tenant, &key, account.clone(), "fixture"))
            .await
            .unwrap();
        let claim = store.claim(&run, "worker", now, 120_000).await.unwrap();
        config.run = run.clone();
        if matches!(case, OutcomeCase::PermissionDenied) {
            config.run.id = "different-run".into();
        }
        if matches!(
            case,
            OutcomeCase::ResponseByteLimit | OutcomeCase::OversizedRejection
        ) {
            config.max_response_bytes = 1024;
        } else if wire.image() {
            config.max_response_bytes = runtrell::images::ImageLimits::default().max_response_bytes;
        }
        let input = match case {
            OutcomeCase::RequestByteLimit => {
                config.max_request_bytes = 1;
                json!({"prompt":"draw"})
            }
            OutcomeCase::InvalidToolInput => json!({"prompt":"draw","size":"invalid"}),
            _ => json!({"prompt":"draw"}),
        };
        let mut dispatch = DispatchRequest::new(
            "effect",
            kind,
            "fixture",
            input.clone(),
            Amount::new(20, 20),
        );
        dispatch.timeout = matches!(case, OutcomeCase::DeadlineBeforePoll).then_some(0);
        if wire.image() {
            dispatch.account_binding = "images".into();
        }
        let runtime = Runtime {
            store: store.clone(),
            model: FakeModel::default(),
            tools: Vec::<BusinessTool>::new(),
            policy: FakePolicy::default(),
            clock,
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let receipt = runtime
            .dispatch(
                &claim,
                dispatch.clone(),
                "invoke",
                |context| invoke(store.clone(), wire, config, context, input, case),
                |_| async { Ok(Lookup::Unknown) },
            )
            .await
            .unwrap_or_else(|error| panic!("{wire:?} {kind:?} {case:?}: {error:?}"));
        assert_receipt(case, wire, &receipt);
        let op = store.operation(&run, "effect").await.unwrap().unwrap();
        assert_eq!(op.result.as_ref(), Some(&receipt));
        let account_record = store.account(&account).await.unwrap();
        let unknown = matches!(receipt.usage_outcome(), UsageOutcome::Unresolved { .. });
        assert_eq!(op.settled, !unknown);
        assert_eq!(
            account_record.reserved,
            if unknown {
                Amount::new(20, 20)
            } else {
                Amount::default()
            }
        );
        assert_eq!(
            account_record.used,
            match receipt.usage_outcome() {
                UsageOutcome::Measured(usage) => usage.amount().unwrap(),
                _ => Amount::default(),
            }
        );
        let issue = match case {
            case if wire.unproven_rejection_usage(case) => Some(UsageIssue::Missing),
            OutcomeCase::FailureWithPartialUsage => Some(UsageIssue::Incomplete),
            OutcomeCase::FailureWithoutUsage | OutcomeCase::TruncatedFailure => {
                Some(UsageIssue::Missing)
            }
            OutcomeCase::FailureWithInvalidUsage | OutcomeCase::InvalidSuccessUsage => {
                Some(UsageIssue::Invalid)
            }
            case if ambiguity_reason(case).is_some() => Some(UsageIssue::OutcomeUnresolved),
            _ => None,
        };
        let expected = issue
            .into_iter()
            .map(|reason| OutcomeException {
                key: "effect".into(),
                kind,
                attempt: 1,
                state: if reason == UsageIssue::OutcomeUnresolved {
                    OperationState::Unknown
                } else if matches!(case, OutcomeCase::InvalidSuccessUsage) {
                    OperationState::Applied
                } else {
                    OperationState::Failed
                },
                ambiguity: ambiguity_reason(case),
                usage_issue: Some(reason),
                unknown_usage: true,
                unsettled: true,
                operator_action: kind == OperationKind::Tool && ambiguity_reason(case).is_none(),
            })
            .collect::<Vec<_>>();
        assert_no_unexpected_ambiguity(store.as_ref(), &run, &expected, false).await;
        if unknown && receipt.ambiguity().is_none() {
            let recovered = runtime
                .dispatch(
                    &claim,
                    dispatch,
                    "invoke",
                    |_| async { panic!("lookup can never resend") },
                    |_| async { Ok(Lookup::Unavailable(LookupFailure::Rejected(401))) },
                )
                .await
                .unwrap();
            assert_eq!(recovered, receipt);
            let saved = store.operation(&run, "effect").await.unwrap().unwrap();
            assert_eq!(saved.result, Some(receipt));
            assert_eq!(saved.lookup_failure, Some(LookupFailure::Rejected(401)));
        }
        if !expected.is_empty() {
            runtrell::test_outcomes::expect(
                &run,
                runtrell::test_outcomes::ExpectedOutcome {
                    exceptions: expected,
                    ..Default::default()
                },
            );
        }
        if let Some(spy) = spy {
            assert!(
                tokio::time::timeout(Duration::from_millis(20), spy.accept())
                    .await
                    .is_err(),
                "pre-send failure reached the server: {wire:?} {case:?}"
            );
        }
        if let Some(server) = server {
            let requests = server.await.unwrap();
            let no_request = matches!(case, OutcomeCase::Tls);
            assert_eq!(
                requests.len(),
                usize::from(!no_request),
                "{wire:?} {case:?}"
            );
            if matches!(case, OutcomeCase::Disconnect) {
                let payload = requests[0].split_once("\r\n\r\n").unwrap().1;
                assert!(serde_json::from_str::<Value>(payload).unwrap().is_object());
            }
        }
    }
}

async fn all_wires<S: ControlStore>(store: Arc<S>) {
    // The independent features also compile and run their own contract rows.
    #[cfg(feature = "openai")]
    tokio::join!(
        contract(store.clone(), Wire::Responses, OperationKind::Model),
        contract(store.clone(), Wire::Responses, OperationKind::Compaction),
        contract(store.clone(), Wire::Images, OperationKind::Tool),
        contract(store.clone(), Wire::ImageEdit, OperationKind::Tool)
    );
    #[cfg(feature = "openrouter")]
    contract(store.clone(), Wire::Chat, OperationKind::Model).await;
    #[cfg(feature = "gemini")]
    tokio::join!(
        contract(store.clone(), Wire::GenerateContent, OperationKind::Model),
        contract(store.clone(), Wire::GenerateImages, OperationKind::Tool),
        contract(store.clone(), Wire::GenerateImageEdit, OperationKind::Tool)
    );
}

fn matrix_capacity() -> Capacity {
    let mut capacity = Capacity::default();
    // Earlier rows intentionally retain reservations and their operation slots.
    for limits in [&mut capacity.global, &mut capacity.tenant] {
        for kind in [
            OperationKind::Run,
            OperationKind::Model,
            OperationKind::Tool,
            OperationKind::Compaction,
        ] {
            limits.insert(kind, OUTCOME_CASES.len() * 8);
        }
    }
    capacity
}

#[tokio::test]
async fn outcome_matrix_memory() {
    let _outcomes = runtrell::test_outcomes::guard();
    all_wires(Arc::new(runtrell::memory::MemoryStore::new(
        matrix_capacity(),
    )))
    .await;
}

#[tokio::test]
async fn outcome_matrix_postgres() {
    let outcomes = runtrell::test_outcomes::guard();
    let url = std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("disposable test database");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let schema: String = sqlx::query_scalar(
        "SELECT 'outcome_contract_' || replace(gen_random_uuid()::text, '-', '')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let store = runtrell_postgres::PgStore::initialize(pool.clone(), &schema, matrix_capacity())
        .await
        .unwrap();
    all_wires(Arc::new(store)).await;
    // The outcome guard reads the durable rows when it is dropped.
    drop(outcomes);
    sqlx::query(&format!("DROP SCHEMA \"{schema}\" CASCADE"))
        .execute(&pool)
        .await
        .unwrap();
}
