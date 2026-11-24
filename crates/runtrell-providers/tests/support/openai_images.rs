// SPDX-License-Identifier: Apache-2.0
// Included only in the OpenAI image acceptance module.
use super::still_images::encoded_output;

#[tokio::test]
async fn unsupported_image_options_name_the_field_and_offer_a_correction() {
    let _outcomes = runtrell::test_outcomes::guard();
    let (mut tool, ctx) = fixture(
        image_config("http://127.0.0.1:1/test".into()),
        ImageAction::Generate,
    )
    .await;
    tool.resolver.limits.allowed_mime.push("image/webp".into());
    let run = tool.runs.get(&ctx.claim.run).await.unwrap();
    for (field, value, allowed) in [
        ("output_format", json!("webp"), "png"),
        ("quality", json!("high"), "omit"),
        ("complexity", json!("medium"), "omit"),
        ("background", json!("opaque"), "omit"),
    ] {
        let mut input = json!({"prompt":"Draw a labeled diagram","size":"1024x1024"});
        input[field] = value;
        let error = tool.quote(&run, &input).await.unwrap_err();
        assert!(
            matches!(&error, Error::InvalidToolInput { path, hint }
                if path == &format!("/{field}") && hint.to_lowercase().contains(allowed)),
            "{field}: {error:?}"
        );
    }
}

#[tokio::test]
async fn img26_optional_raster_fields_reach_the_wire() {
    let _outcomes = runtrell::test_outcomes::guard();
    let (url, http) = server(200, "", image_response()).await;
    let mut cfg = image_config(url);
    cfg.capabilities.limits.model = "gpt-image-2.6-sunburst".into();
    cfg.capabilities.limits.price.as_mut().unwrap().model = cfg.capabilities.limits.model.clone();
    let (tool, ctx) = fixture(cfg, ImageAction::Generate).await;
    let input = json!({"prompt":"draw","quality":"low","complexity":"medium","background":"transparent"});
    assert!(matches!(tool.invoke(&ctx, input).await.unwrap(), ExternalResult::Applied { .. }));
    let sent = http.await.unwrap();
    assert_eq!(sent.len(), 1);
    let wire: Value = serde_json::from_str(sent[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(wire, json!({"model":"gpt-image-2.6-sunburst","prompt":"draw","n":1,"output_format":"png","quality":"low","complexity":"medium","background":"transparent"}));
}

#[tokio::test]
async fn openai_generated_artifact_is_reinput_to_the_model_http_adapter() {
    let _outcomes = runtrell::test_outcomes::guard();
    let (endpoint, generation) = server(200, "", image_response()).await;
    let (tool, ctx) = fixture(image_config(endpoint), ImageAction::Generate).await;
    let ExternalResult::Applied { output, .. } =
        tool.invoke(&ctx, json!({"prompt":"draw"})).await.unwrap()
    else {
        panic!("expected generated artifact")
    };
    let images: Vec<ImageReference> =
        serde_json::from_value(output["image_inputs"].clone()).unwrap();
    let (endpoint, input) = server(200, "", response("openai", "completed", false)).await;
    let mut cfg = image_config(endpoint.clone());
    cfg.endpoint = endpoint;
    cfg.run = ctx.claim.run.clone();
    let model = ImageModel {
        inner: OpenAi::new(cfg).unwrap(),
        resolver: ImageResolver {
            store: tool.resolver.store.clone(),
            policy: FakePolicy::default(),
            accesses: vec![tool.output_access.clone()],
            limits: ImageLimits::default(),
        },
        runs: tool.runs.clone(),
    };
    let run = tool.runs.get(&ctx.claim.run).await.unwrap();
    let mut request = request();
    request.images = images;
    let prepared = model.prepare_request(&run, request).await.unwrap();
    model.input_upper_bound(&prepared).unwrap();
    assert!(matches!(
        model.invoke(&ctx, prepared).await.unwrap(),
        ExternalResult::Applied { .. }
    ));
    assert_eq!(generation.await.unwrap().len(), 1);
    let sent = input.await.unwrap();
    assert_eq!(sent.len(), 1);
    let body: Value = serde_json::from_str(sent[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(
        body["input"][1]["content"][0],
        json!({"type":"input_image","image_url":format!("data:image/png;base64,{}",STANDARD.encode(png()))})
    );
}

#[tokio::test]
async fn openai_size_output_formats_are_saved_and_the_wire_has_only_inherited_fields() {
    let _outcomes = runtrell::test_outcomes::guard();
    for format in ["png", "jpeg", "webp"] {
        let mut body = image_response();
        let encoded = encoded_output(format);
        body["data"][0]["b64_json"] = json!(encoded);
        let (url, task) = server(200, "", body).await;
        let mut cfg = image_config(url);
        cfg.image_limits.allowed_mime = vec![format!("image/{format}")];
        let (mut tool, ctx) = fixture(cfg, ImageAction::Generate).await;
        tool.resolver.limits.allowed_mime = vec![format!("image/{format}")];
        let request = json!({"prompt":"draw","size":"1024x1024","output_format":format});
        let ExternalResult::Applied { output, .. } = tool.invoke(&ctx, request).await.unwrap()
        else {
            panic!("expected image in {format}")
        };
        let refs: Vec<ImageReference> =
            serde_json::from_value(output["image_inputs"].clone()).unwrap();
        assert_eq!(refs[0].mime(), format!("image/{format}"));
        let run = tool.runs.get(&ctx.claim.run).await.unwrap();
        let resolved = tool.resolver.resolve(&run, &refs).await.unwrap();
        assert_eq!(resolved[0].data_base64(), encoded);
        let sent = task.await.unwrap();
        assert_eq!(sent.len(), 1);
        let wire: Value = serde_json::from_str(sent[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(
            wire,
            json!({"model":"host-model","prompt":"draw","n":1,"size":"1024x1024","output_format":format})
        );
        let bytes = STANDARD.decode(&encoded).unwrap();
        for end in 0..bytes.len() {
            assert!(
                tool.resolver
                    .limits
                    .inspect(refs[0].mime(), &bytes[..end])
                    .is_err(),
                "accepted truncated {format} at {end}"
            );
        }
    }
}

#[tokio::test]
async fn openai_absent_partial_and_text_usage_stay_reported_or_unreported() {
    let _outcomes = runtrell::test_outcomes::guard();
    for reported in [
        Value::Null,
        json!({"input_tokens":3}),
        json!({"input_tokens":3,"output_tokens":5,"input_tokens_details":{"text_tokens":1,"image_tokens":2},"output_tokens_details":{"text_tokens":1,"image_tokens":4}}),
    ] {
        let mut body = image_response();
        body["usage"] = reported.clone();
        let (url, task) = server(200, "", body).await;
        let (tool, ctx) = fixture(image_config(url), ImageAction::Generate).await;
        let result = tool.invoke(&ctx, json!({"prompt":"draw"})).await.unwrap();
        let ExternalResult::Applied { usage, .. } = &result else {
            panic!("expected saved image")
        };
        assert_eq!(usage.input, reported["input_tokens"].as_u64());
        assert_eq!(usage.output, reported["output_tokens"].as_u64());
        assert_eq!(
            usage.quantities.get("text_output_tokens").copied(),
            reported["output_tokens_details"]["text_tokens"].as_u64()
        );
        assert_eq!(
            usage.image_output,
            reported["output_tokens_details"]["image_tokens"].as_u64()
        );
        assert_eq!(usage.cached, None);
        assert_eq!(usage.reasoning, None);
        if usage.output.is_none() {
            assert_eq!(usage.micro_usd, None);
            assert_eq!(usage.amount(), Err(Error::UsageUnknown));
        }
        assert_eq!(
            tool.lookup(&ctx, json!({"prompt":"draw"})).await.unwrap(),
            Lookup::Found(result)
        );
        assert_eq!(task.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn openai_malformed_single_result_and_http_spending_errors_keep_usage() {
    let _outcomes = runtrell::test_outcomes::guard();
    for data in [
        Value::Null,
        json!([]),
        json!([{}, {}]),
        json!([{}]),
        json!([{"url":"https://must-not-fetch.example/image"}]),
        json!([{"b64_json":""}]),
        json!([{"b64_json":17}]),
    ] {
        let mut body = image_response();
        body["data"] = data;
        let (url, task) = server(200, "", body).await;
        let (tool, ctx) = fixture(image_config(url), ImageAction::Generate).await;
        let ExternalResult::Failed { usage, .. } =
            tool.invoke(&ctx, json!({"prompt":"draw"})).await.unwrap()
        else {
            panic!("malformed response accepted")
        };
        assert_eq!(usage.amount().unwrap(), Amount::new(8, 8));
        assert_eq!(task.await.unwrap().len(), 1);
    }
    for code in [
        "insufficient_quota",
        "insufficient_credits",
        "billing_hard_limit_reached",
    ] {
        let mut body = image_response();
        body["error"] = json!({"code":code,"message":"TEST-KEY-DO-NOT-PRINT"});
        let (url, task) = server(429, "Retry-After: 2\r\n", body).await;
        let (tool, ctx) = fixture(image_config(url), ImageAction::Generate).await;
        let result = tool.invoke(&ctx, json!({"prompt":"draw"})).await.unwrap();
        let ExternalResult::Failed {
            code,
            usage,
            retry_after,
            ..
        } = result
        else {
            panic!("expected paid HTTP failure")
        };
        let failure: Failure = serde_json::from_str(&code).unwrap();
        assert_eq!(failure.class, ErrorClass::SpendingLimit);
        assert_eq!(retry_after, None);
        assert_eq!(failure.retry_after.as_deref(), Some("2"));
        assert_eq!(usage.amount().unwrap(), Amount::new(8, 8));
        assert!(!code.contains("TEST-KEY"));
        assert_eq!(task.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn managed_openai_image_rejection_releases_reservation_and_returns_feedback() {
    let _outcomes = runtrell::test_outcomes::guard();
    managed_image_feedback(false).await;
}

#[tokio::test]
async fn managed_image_pre_send_failure_returns_retryable_feedback() {
    let _outcomes = runtrell::test_outcomes::guard();
    managed_image_feedback(true).await;
}

async fn managed_image_feedback(pre_send: bool) {
    use runtrell_testkit::FakeModel;

    let (url, http) = server(
        400,
        "x-request-id: req_invalid_size\r\n",
        json!({"error":{
            "type":"invalid_request_error","code":"invalid_value","param":"size",
            "message":"TEST-KEY-DO-NOT-PRINT"
        }}),
    )
    .await;
    let (mut tool, ctx) = fixture(image_config(url.clone()), ImageAction::Generate).await;
    let handle = ctx.claim.run;
    let transport_clock = tool.clock.clone();
    let mut provider = Images::connect(
        "openai/gpt-image-2.6-sunburst",
        models::OperationKind::RasterGenerate,
        None,
        Connection {
            run: handle.clone(),
            session: None,
            credentials: Arc::new(move |_| {
                if pre_send {
                    transport_clock.set(transport_clock.now() + 233);
                    return Err(Error::Deadline);
                }
                Ok(ApiKey::new("TEST-KEY-DO-NOT-PRINT"))
            }),
            attachment_resolver: None,
            retry_policy: RetryPolicy::default(),
            timeout_ms: 10_000,
            context_cap: None,
            input_token_cap: None,
            max_output_tokens: None,
            reasoning: None,
        },
    )
    .unwrap();
    provider.set_fixture_endpoint(image_config(url).endpoint);
    tool.provider = provider;
    tool.config.reserve = Amount::default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    tool.clock.set(now);
    let runs = tool.runs.clone();
    let account = runs.get(&handle).await.unwrap().spec.account;
    let claim = runs.claim(&handle, "worker", now, 100_000).await.unwrap();
    let mut arguments = json!({"prompt":"Draw a labeled diagram","size":"1024x1024",
                              "output_format":"png","complexity":"medium","background":"opaque","quality":"auto"});
    let run = runs.get(&handle).await.unwrap();
    assert!(matches!(
        tool.quote(&run, &arguments).await,
        Err(Error::InvalidToolInput { path, hint })
            if path == "/quality" && ["low", "medium", "high"].iter().all(|q| hint.contains(q))
    ));
    arguments["quality"] = json!("low");
    let runtime = Runtime {
        store: runs.clone(),
        model: FakeModel::scripted(vec![
            FakeModel::calls(vec![ToolCall {
                id: "draw".into(),
                name: "image.generate".into(),
                arguments,
            }]),
            FakeModel::decision(json!({"decision":"answered"})),
        ]),
        policy: FakePolicy::default(),
        clock: tool.clock.clone(),
        tools: vec![tool],
        validator: Arc::new(DecisionValidator::default()),
        finalizer: Arc::new(DecisionFinalizer),
    };
    let run = runtime.drive(&claim).await.unwrap();
    let operations = runs.list_operations(&handle).await.unwrap();
    let op = operations
        .iter()
        .find(|op| op.request.target == "image.generate")
        .unwrap();
    assert_eq!(
        serde_json::to_value(op.request.pricing.as_ref().unwrap()).unwrap()["price_id"],
        "sunburst-generate-tokens"
    );
    assert!(op.request.reserve.micro_usd > 0);
    assert_eq!(op.send_count, 1);
    assert!(op.settled && !op.unknown_usage, "{op:?}");
    let diagnostic = op.provider_diagnostic().unwrap();
    if pre_send {
        assert_eq!(op.usage_outcome(), UsageOutcome::NonBillable(NonBillableReason::RetryableNotSent));
        assert_eq!(diagnostic.phase, ProviderFailurePhase::PreSend);
        assert_eq!(diagnostic.send_certainty, ProviderSendCertainty::DefinitelyNotSent);
    } else {
    assert!(matches!(
        &op.result,
        Some(ExternalResult::Failed { usage, repair_hint: Some(_), .. })
            if usage.amount() == Ok(Amount::default())
    ));
    let diagnostic = serde_json::to_value(diagnostic).unwrap();
    assert_eq!(diagnostic["http_status"], 400);
    assert_eq!(diagnostic["provider_type"], "invalid_request_error");
    assert_eq!(diagnostic["provider_code"], "invalid_value");
    assert_eq!(diagnostic["request_id"], "req_invalid_size");
    }
    let totals = runs.account(&account).await.unwrap();
    assert_eq!(totals.reserved, Amount::default());
    assert_eq!(totals.unknown, 0);
    assert_eq!(totals.used, Amount::new(8, 8));
    assert_eq!(run.state, RunState::Terminal(Terminal::Succeeded));
    {
        let requests = runtime.model.requests.lock().unwrap();
        let feedback = requests[1]
            .messages
            .iter()
            .find(|m| m.role == "tool")
            .unwrap();
        assert_eq!(feedback.data["repairable"], true);
        if pre_send {
            assert_eq!(feedback.data["retryable"], true);
            assert_eq!(feedback.data["code"], "CREDENTIAL_UNAVAILABLE");
        } else {
            assert!(feedback.data["hint"].as_str().unwrap().contains("size"));
        }
        assert!(!feedback.data.to_string().contains("TEST-KEY"));
        assert!(!feedback.data.to_string().contains("req_invalid_size"));
    }
    let requests = http.await.unwrap();
    assert_eq!(requests.len(), usize::from(!pre_send));
    if let Some(request) = requests.first() {
        assert!(request.starts_with("POST /v1/images/generations "));
    }
    runtrell_testkit::outcome_contracts::assert_no_unexpected_ambiguity(
        runs.as_ref(), &handle, &[], false,
    ).await;
}

#[tokio::test]
async fn openai_failed_image_responses_preserve_transport_evidence() {
    let _outcomes = runtrell::test_outcomes::guard();
    for (raw, phase, status, request_id) in [
        ("", ProviderFailurePhase::Send, None, Value::Null),
        (
            "HTTP/1.1 200 OK\r\nx-request-id: req_truncated\r\nContent-Length: 20\r\n\r\n{",
            ProviderFailurePhase::ResponseRead,
            Some(200),
            json!("req_truncated"),
        ),
        (
            "HTTP/1.1 200 OK\r\nx-request-id: req_invalid\r\nContent-Length: 4\r\n\r\nnope",
            ProviderFailurePhase::ResponseDecode,
            Some(200),
            json!("req_invalid"),
        ),
    ] {
        let (url, http) = raw_server(raw.into()).await;
        let (tool, ctx) = fixture(image_config(url), ImageAction::Generate).await;
        let result = tool.invoke(&ctx, json!({"prompt":"draw"})).await.unwrap();
        let ExternalResult::Ambiguous(_) = &result else {
            panic!("expected an ambiguous outcome");
        };
    let diagnostic = result.provider_diagnostic().unwrap();
        assert_eq!(diagnostic.phase, phase);
        assert_eq!(diagnostic.http_status, status);
        assert_eq!(
            serde_json::to_value(diagnostic).unwrap()["request_id"],
            request_id
        );
        assert_eq!(http.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn openai_image_size_error_code_can_guide_repair_without_param() {
    let _outcomes = runtrell::test_outcomes::guard();
    let (url, http) = server(
        400,
        "",
        json!({"error":{"type":"invalid_request_error","code":"invalid_size"}}),
    )
    .await;
    let (tool, ctx) = fixture(image_config(url), ImageAction::Generate).await;
    let result = tool.invoke(&ctx, json!({"prompt":"draw"})).await.unwrap();
    assert!(matches!(
        &result,
        ExternalResult::Failed { usage, repair_hint: Some(hint), .. }
            if usage.amount() == Ok(Amount::default()) && hint.contains("size")
    ));
    assert_eq!(http.await.unwrap().len(), 1);
}

#[tokio::test]
async fn openai_image_timeout_after_request_body_remains_unknown() {
    let _outcomes = runtrell::test_outcomes::guard();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/test", listener.local_addr().unwrap());
    let http = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = vec![];
        stream.read_to_end(&mut bytes).await.unwrap();
        String::from_utf8(bytes).unwrap()
    });
    let mut cfg = image_config(url);
    cfg.capabilities.limits.timeout = 250;
    let (tool, ctx) = fixture(cfg, ImageAction::Generate).await;
    let result = tool.invoke(&ctx, json!({"prompt":"draw"})).await.unwrap();
    let ExternalResult::Ambiguous(_) = &result else {
        panic!("expected an ambiguous outcome");
    };
    let diagnostic = result.provider_diagnostic().unwrap();
    assert_eq!(diagnostic.phase, ProviderFailurePhase::Send);
    assert_eq!(diagnostic.send_certainty, ProviderSendCertainty::Unknown);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), http)
            .await
            .unwrap()
            .unwrap()
            .contains("\"prompt\":\"draw\"")
    );
}

#[tokio::test]
async fn openai_image_diagnostics_omit_echoed_credentials_and_unsafe_ids() {
    let _outcomes = runtrell::test_outcomes::guard();
    let (url, http) = server(
        422,
        "x-request-id: TEST-KEY-DO-NOT-PRINT\r\n",
        json!({"error":{
            "type":"invalid_request_error","code":"TEST-KEY-DO-NOT-PRINT",
            "param":"arbitrary provider text","message":"unfiltered error message"
        }}),
    )
    .await;
    let (tool, ctx) = fixture(image_config(url), ImageAction::Generate).await;
    let result = tool.invoke(&ctx, json!({"prompt":"draw"})).await.unwrap();
    assert!(matches!(
        &result,
        ExternalResult::Failed { usage, repair_hint: None, .. }
            if usage.amount() == Ok(Amount::default())
    ));
    let diagnostic = result.provider_diagnostic().unwrap();
    assert_eq!(diagnostic.http_status, Some(422));
    assert_eq!(
        diagnostic.provider_type.as_deref(),
        Some("invalid_request_error")
    );
    assert_eq!(diagnostic.provider_code, None);
    assert_eq!(diagnostic.provider_param, None);
    assert_eq!(diagnostic.request_id, None);
    let stored = serde_json::to_string(&result).unwrap();
    assert!(!stored.contains("TEST-KEY-DO-NOT-PRINT"));
    assert!(!stored.contains("unfiltered error message"));
    assert_eq!(http.await.unwrap().len(), 1);
}

#[tokio::test]
async fn openai_unreachable_image_endpoint_is_a_known_no_send_failure() {
    let _outcomes = runtrell::test_outcomes::guard();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/test", listener.local_addr().unwrap());
    drop(listener);
    let (tool, ctx) = fixture(image_config(url), ImageAction::Generate).await;
    let result = tool.invoke(&ctx, json!({"prompt":"draw"})).await.unwrap();
    assert!(
        matches!(
            &result,
            ExternalResult::Failed { usage, repair_hint: None, .. }
                if usage.amount() == Ok(Amount::default())
        ),
        "{result:?}"
    );
    let diagnostic = result.provider_diagnostic().unwrap();
    assert_eq!(diagnostic.phase, ProviderFailurePhase::PreSend);
    assert_eq!(
        diagnostic.send_certainty,
        ProviderSendCertainty::DefinitelyNotSent
    );
    assert_eq!(diagnostic.http_status, None);
}

#[tokio::test]
async fn openai_mask_foreign_hash_and_output_options_are_rejected_before_send() {
    let _outcomes = runtrell::test_outcomes::guard();
    let (url, task) = server(200, "", image_response()).await;
    let (mut tool, mut ctx) = fixture(image_config(url), ImageAction::Edit).await;
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
    let mut bad = reference.clone();
    bad.hash = "forged".into();
    assert!(
        tool.invoke(
            &ctx,
            json!({"prompt":"edit","references":[ImageReference::Artifact(bad)]})
        )
        .await
        .is_err()
    );
    let mut foreign = reference.clone();
    foreign.owner.owner.tenant = "another-tenant".into();
    assert_eq!(
        tool.invoke(
            &ctx,
            json!({"prompt":"edit","references":[ImageReference::Artifact(foreign)]})
        )
        .await
        .unwrap_err(),
        Error::PermissionDenied
    );
    tool.config.action = ImageAction::MaskEdit;
    let input = json!({"prompt":"edit","references":[ImageReference::Artifact(reference.clone())],"mask":ImageReference::Artifact(reference)});
    assert!(matches!(
        tool.invoke(&ctx, input).await,
        Err(Error::Unsupported(_))
    ));
    tool.config.action = ImageAction::Generate;
    ctx.permission = tool.descriptor().permission;
    for input in [
        json!({"prompt":"draw","output_format":"gif"}),
        json!({"prompt":"draw","output_format":"jpeg"}),
        json!({"prompt":"draw","quality":"high"}),
        json!({"prompt":"draw","model":"untrusted"}),
        json!({"prompt":"draw","n":2}),
        json!({"prompt":"draw","size":"x".repeat(129)}),
        json!({"prompt":"draw","size":"8193x1"}),
        json!({"prompt":"draw","size":"4097x4097"}),
    ] {
        assert!(tool.invoke(&ctx, input).await.is_err());
    }
    assert!(task.await.unwrap().is_empty());
}

#[tokio::test]
async fn pg_openai_image_failures_settle_or_hold_with_provider_evidence() {
    let _outcomes = runtrell::test_outcomes::guard();
    use runtrell::test_outcomes::{ExpectedOutcome, OutcomeException, expect};
    use runtrell_postgres::PgStore;
    use runtrell_testkit::FakeModel;
    let database =
        std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("OpenAI Pg acceptance never skips");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&database)
        .await
        .unwrap();
    for case in [
        "base64",
        "structure",
        "bytes",
        "dimensions",
        "count",
        "absent",
        "partial",
        "disconnect",
        "authentication",
        "server_error",
    ] {
        let tick = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let schema = format!("openai_image_failure_{}", tick.as_nanos());
        let runs = Arc::new(
            PgStore::initialize(pool.clone(), &schema, Capacity::default())
                .await
                .unwrap(),
        );
        let account = runs
            .create_account("tenant", Amount::new(10000, 10000), None)
            .await
            .unwrap();
        let handle = runs
            .start(RunSpec::new("tenant", case, account.clone(), "draw"))
            .await
            .unwrap();
        let claim = runs
            .claim(&handle, "worker", tick.as_millis() as u64, 100)
            .await
            .unwrap();
        let access = Access::owner(Namespace {
            owner: handle.clone(),
            agent: "main".into(),
        });
        let mut body = image_response();
        match case {
            "base64" => body["data"][0]["b64_json"] = json!("???"),
            "structure" => body["data"][0]["b64_json"] = json!(STANDARD.encode(b"invalid PNG")),
            "dimensions" => {
                let mut bytes = png();
                bytes[19] = 2;
                let mut crc = !0u32;
                for b in &bytes[12..29] {
                    crc ^= u32::from(*b);
                    for _ in 0..8 {
                        crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
                    }
                }
                bytes[29..33].copy_from_slice(&(!crc).to_be_bytes());
                body["data"][0]["b64_json"] = json!(STANDARD.encode(bytes));
            }
            "count" => body["data"] = json!(vec![body["data"][0].clone(); 2]),
            "absent" => {
                body.as_object_mut().unwrap().remove("usage");
            }
            "partial" => body["usage"] = json!({"input_tokens":3}),
            _ => (),
        }
        let (endpoint, http) = match case {
            "disconnect" => raw_server(
                "HTTP/1.1 200 OK\r\nx-request-id: req_pg_image\r\nContent-Length: 900\r\nConnection: close\r\n\r\n{}".into(),
            ).await,
            "authentication" => server(
                401,
                "x-request-id: req_pg_image\r\n",
                json!({"error":{"type":"invalid_request_error","code":"invalid_api_key"}}),
            ).await,
            "server_error" => server(
                500,
                "x-request-id: req_pg_image\r\n",
                json!({"error":{"type":"server_error","code":"internal_error"}}),
            ).await,
            _ => server(200, "x-request-id: req_pg_image\r\n", body).await,
        };
        let mut cfg = image_config(endpoint);
        cfg.run = handle.clone();
        if case == "bytes" {
            cfg.image_limits.max_image_bytes = 16;
        }
        if case == "dimensions" {
            cfg.image_limits.max_pixels = 1;
        }
        // For a dimensions failure use a CRC-valid PNG whose header claims 2x1.
        // The response and common validator use the same pinned image bounds.
        if case == "dimensions" {
            cfg.image_limits.max_width = 1;
            cfg.image_limits.max_height = 1;
        }
        let clock = TestClock::default();
        clock.set(tick.as_millis() as u64);
        let data = DataStore::memory(StoragePolicy::default());
        let rt = Runtime {
            store: runs.clone(),
            model: FakeModel::scripted(vec![FakeModel::calls(vec![ToolCall {
                id: "gen".into(),
                name: "image.generate".into(),
                arguments: json!({"prompt":"draw"}),
            }])]),
            tools: vec![ImageTool {
                provider: Images::new(cfg).unwrap(),
                resolver: ImageResolver {
                    store: data,
                    policy: FakePolicy::default(),
                    accesses: vec![access.clone()],
                    limits: ImageLimits::default(),
                },
                runs: runs.clone(),
                clock: clock.clone(),
                output_access: access,
                config: ImageToolConfig {
                    action: ImageAction::Generate,
                    reserve: Amount::new(2048, 2048),
                    timeout: 30_000,
                },
            }],
            policy: FakePolicy::default(),
            clock,
            validator: Arc::new(DecisionValidator::default()),
            finalizer: Arc::new(DecisionFinalizer),
        };
        let run = rt.drive(&claim).await.unwrap();
        let operations = runs.list_operations(&handle).await.unwrap();
        let tool_op = operations
            .iter()
            .find(|op| op.request.kind == OperationKind::Tool)
            .unwrap();
        assert_eq!(tool_op.send_count, 1);
        let unknown = matches!(case, "absent" | "partial" | "disconnect" | "server_error");
        if !matches!(case, "absent" | "partial") {
            let diagnostic = tool_op.provider_diagnostic().unwrap();
            let status = match case {
                "authentication" => 401,
                "server_error" => 500,
                _ => 200,
            };
            assert_eq!(diagnostic.http_status, Some(status));
            assert_eq!(diagnostic.request_id.as_deref(), Some("req_pg_image"));
        }
        let (settled,unsettled):(i64,i64) = sqlx::query_as(&format!("SELECT count(*) FILTER (WHERE settled AND NOT unknown_usage), count(*) FILTER (WHERE unknown_usage) FROM {schema}.attempts")).fetch_one(&pool).await.unwrap();
        let balance = runs.account(&account).await.unwrap();
        if unknown {
            assert!(matches!(run.state, RunState::Waiting(_)));
            assert!(!tool_op.settled);
            assert_eq!((settled, unsettled), (1, 1));
            assert_eq!(balance.unknown, 1);
            assert_eq!(balance.reserved, Amount::new(2048, 2048));
            assert_eq!(balance.used, Amount::new(4, 4));
            // Recovery reads the saved receipt (or remains Unknown after a lost
            // HTTP body); it cannot authorize a second generation.
            let _ = rt.drive(&claim).await.unwrap();
            assert_eq!(runs.account(&account).await.unwrap(), balance);
            let persisted = runs
                .list_operations(&handle)
                .await
                .unwrap()
                .into_iter()
                .find(|op| op.id == tool_op.id)
                .unwrap();
            assert_eq!(
                persisted.provider_diagnostic(),
                tool_op.provider_diagnostic()
            );
            let expected = if case == "disconnect" {
                ExpectedOutcome {
                    exceptions: vec![OutcomeException::unresolved(
                        "tool:1:gen", OperationKind::Tool, AmbiguityReason::ResponseLost,
                    )],
                    terminal_unknown: true,
                    ..Default::default()
                }
            } else {
                let (state, issue) = match case {
                    "absent" => (OperationState::Applied, UsageIssue::Missing),
                    "partial" => (OperationState::Applied, UsageIssue::Incomplete),
                    "server_error" => (OperationState::Failed, UsageIssue::Missing),
                    _ => unreachable!("unexpected unresolved fixture"),
                };
                ExpectedOutcome {
                    exceptions: vec![OutcomeException {
                        operator_action: true,
                        ..OutcomeException::unresolved_usage("tool:1:gen", OperationKind::Tool, state, issue)
                    }],
                    operator_wait: true,
                    ..Default::default()
                }
            };
            expect(&handle, expected);
        } else {
            assert!(matches!(run.state, RunState::Terminal(Terminal::Failed(_))));
            assert!(tool_op.settled);
            let cost = if case == "authentication" { 0 } else { 8 };
            assert!(
                matches!(&tool_op.result,Some(ExternalResult::Failed { usage,.. }) if usage.amount() == Ok(Amount::new(cost,cost)))
            );
            assert_eq!((settled, unsettled), (2, 0));
            assert_eq!(balance.used, Amount::new(4 + cost, 4 + cost));
            assert_eq!(balance.reserved, Amount::default());
        }
        assert_eq!(http.await.unwrap().len(), 1);
    }
}
