// SPDX-License-Identifier: Apache-2.0
// Included only in the OpenAI image acceptance module.
use crate::still_images::encoded_output;

#[tokio::test]
async fn openai_generated_artifact_is_reinput_to_the_model_http_adapter() {
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
        assert_eq!(retry_after, Some(2));
        assert_eq!(usage.amount().unwrap(), Amount::new(8, 8));
        assert!(!code.contains("TEST-KEY"));
        assert_eq!(task.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn openai_mask_foreign_hash_and_output_options_are_rejected_before_send() {
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
async fn pg_openai_paid_failures_settle_once_and_missing_usage_holds_reservation() {
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
    ] {
        let tick = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let schema = format!("openai_image_failure_{}", tick.as_nanos());
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
            .start(RunSpec::new("tenant", case, account.clone(), "draw"))
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
        let (endpoint, http) = if case == "disconnect" {
            raw_server(
                "HTTP/1.1 200 OK\r\nContent-Length: 900\r\nConnection: close\r\n\r\n{}".into(),
            )
            .await
        } else {
            server(200, "", body).await
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
        clock.set(tick.as_secs());
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
                    timeout: 30,
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
        let unknown = matches!(case, "absent" | "partial" | "disconnect");
        let (settled,unsettled):(i64,i64) = sqlx::query_as(&format!("SELECT count(*) FILTER (WHERE settled AND NOT unknown), count(*) FILTER (WHERE unknown) FROM {schema}.budget_entries")).fetch_one(&pool).await.unwrap();
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
        } else {
            assert!(matches!(run.state, RunState::Terminal(Terminal::Failed(_))));
            assert!(tool_op.settled);
            assert!(
                matches!(&tool_op.result,Some(ExternalResult::Failed { usage,.. }) if usage.amount() == Ok(Amount::new(8,8)))
            );
            assert_eq!((settled, unsettled), (2, 0));
            assert_eq!(balance.used, Amount::new(12, 12));
        }
        assert_eq!(http.await.unwrap().len(), 1);
    }
}
