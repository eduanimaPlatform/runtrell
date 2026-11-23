// SPDX-License-Identifier: Apache-2.0
fn img26_config(url: String) -> Config {
    let mut cfg = image_config(url);
    cfg.capabilities.limits.model = "gpt-image-2.6-sunburst".into();
    cfg.capabilities.limits.price.as_mut().unwrap().model = cfg.capabilities.limits.model.clone();
    cfg.capabilities.images.svg_generation = true;
    cfg.capabilities.images.svg_generation_size = Some(ImageSizePolicy::Values {
        sizes: vec!["1024x1024".into(), "1536x1024".into(), "1024x1536".into()],
        auto: true,
    });
    cfg.image_limits.allowed_mime.push("image/svg+xml".into());
    cfg
}

const SVG_SOURCE: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"999999\" height=\"999999\"><text>図 &amp; α</text></svg>\n";

#[tokio::test]
async fn img26_svg_wire_prices_image_and_author_usage() {
    let _outcomes = runtrell::test_outcomes::guard();
    let mut body = image_response();
    body["data"][0]["b64_json"] = json!(STANDARD.encode(SVG_SOURCE));
    body["usage"] = json!({"total_tokens":935,"input_tokens":111,"output_tokens":824,
        "input_tokens_details":{"text_tokens":111,"image_tokens":0,"cached_tokens":0,
            "cached_tokens_details":{"image_tokens":0}},
        "output_tokens_details":{"image_tokens":824,"text_tokens":0}});
    body["svg_usage"] = json!({"model":"gpt-6-luna","input_tokens":1583,
        "output_tokens":2304,"reasoning_tokens":1779,"total_tokens":3887});
    let (url, http) = server(200, "", body).await;
    let mut cfg = img26_config(url);
    cfg.capabilities.limits.model = "gpt-image-2.6-flare".into();
    cfg.capabilities.limits.price = Some(Price {
        model: "gpt-image-2.6-flare".into(),
        input: 5_000_000,
        cached: 1_250_000,
        output: 30_000_000,
        reasoning: 30_000_000,
        image_input: 8_000_000,
        image_output: 30_000_000,
        cached_image_input: 2_000_000,
    });
    cfg.capabilities.images.svg_author_prices = vec![SvgAuthorPrice {
        model: "gpt-6-luna".into(),
        input: 100_000,
        output: 500_000,
        reasoning_in_output: true,
    }];
    let limits = cfg.image_limits.clone();
    let (mut tool, ctx) = fixture(cfg, ImageAction::SvgGenerate).await;
    tool.resolver.limits = limits;
    let input = json!({"prompt":"draw svg","output_format":"svg"});
    let result = tool.invoke(&ctx, input.clone()).await.unwrap();
    let ExternalResult::Applied { usage, .. } = &result else {
        panic!("expected saved result")
    };
    assert_eq!((usage.cached, usage.cached_image_input), (Some(0), Some(0)));
    assert_eq!(
        usage.price_evidence.as_ref().unwrap().output_micro_usd,
        Some(24_720)
    );
    assert_eq!(
        usage.price_evidence.as_ref().unwrap().input_micro_usd,
        Some(555)
    );
    assert_eq!(usage.micro_usd, Some(25_275));
    assert_eq!(usage.svg_usage.as_ref().unwrap().micro_usd, Some(1_311));
    assert_eq!(usage.amount(), Ok(Amount::new(4822, 26_586)));
    assert_eq!(
        tool.lookup(&ctx, input).await.unwrap(),
        Lookup::Found(result)
    );
    assert_eq!(http.await.unwrap().len(), 1);
}

#[tokio::test]
async fn img26_svg_wire_exact_bytes_digest_and_receipt_replay() {
    let _outcomes = runtrell::test_outcomes::guard();
    for options in [
        json!({}),
        json!({"model":"verified-author"}),
        json!({"model":"verified-author","reasoning":{"effort":"high"}}),
        json!({"model":"verified-author","reasoning":{"effort":"max"}}),
        json!({"model":"verified-author","reasoning":{"effort":null}}),
    ] {
        let mut body = image_response();
        body["data"][0]["b64_json"] = json!(STANDARD.encode(SVG_SOURCE));
        body["svg_usage"] = json!({"model":"returned-author","input_tokens":7,"output_tokens":11,"reasoning_tokens":3,"total_tokens":18,"future_field":"preserve"});
        let (url, http) = server(200, "", body.clone()).await;
        let mut cfg = img26_config(url);
        cfg.capabilities.images.svg_model_selection = options.get("model").is_some();
        cfg.image_limits.max_image_bytes = 1;
        cfg.image_limits.max_pixels = 1;
        cfg.image_limits.max_width = 1;
        cfg.image_limits.max_height = 1;
        let limits = cfg.image_limits.clone();
        let (mut tool, ctx) = fixture(cfg, ImageAction::SvgGenerate).await;
        tool.resolver.limits = limits;
        let mut input = json!({"prompt":"draw svg","output_format":"svg","quality":"low","complexity":"medium","background":"transparent","size":"1024x1024"});
        if options != json!({}) {
            input["svg"] = options.clone();
        }
        let result = tool.invoke(&ctx, input.clone()).await.unwrap();
        let ExternalResult::Applied { output, usage, .. } = &result else {
            panic!("SVG not stored: {result:?}");
        };
        assert_eq!(output["image_inputs"], json!([]));
        let refs: Vec<ImageReference> =
            serde_json::from_value(output["svg_outputs"].clone()).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].mime(), "image/svg+xml");
        assert_eq!(refs[0].hash(), content_digest(SVG_SOURCE.as_bytes()));
        let run = tool.runs.get(&ctx.claim.run).await.unwrap();
        let bytes = tool
            .resolver
            .read_reference(
                &run,
                &refs[0],
                "image.retrieve",
                tool.resolver.limits.max_svg_bytes,
            )
            .await
            .unwrap();
        assert_eq!(bytes, SVG_SOURCE.as_bytes());
        assert_eq!(
            usage.svg_usage.as_ref().unwrap().reported,
            body["svg_usage"]
        );
        assert_eq!(usage.svg_usage.as_ref().unwrap().micro_usd, None);
        assert_eq!((usage.input, usage.output), (Some(3), Some(5)));
        assert_eq!(usage.amount(), Err(Error::UsageUnknown));
        assert_eq!(
            tool.lookup(&ctx, input.clone()).await.unwrap(),
            Lookup::Found(result.clone())
        );
        assert_eq!(tool.invoke(&ctx, input).await.unwrap(), result);
        tool.authorize_result(&run, output).await.unwrap();
        let sent = http.await.unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].starts_with("POST /v1/images/generations "));
        let wire: Value = serde_json::from_str(sent[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let mut expected = json!({"model":"gpt-image-2.6-sunburst","prompt":"draw svg","n":1,"output_format":"svg","quality":"low","complexity":"medium","background":"transparent","size":"1024x1024"});
        if options != json!({}) {
            let mut expected_options = options;
            if expected_options["reasoning"]["effort"].is_null()
                && expected_options.get("reasoning").is_some()
            {
                expected_options["reasoning"] = json!({});
            }
            expected["svg"] = expected_options;
        }
        assert_eq!(wire, expected);
    }
}

#[tokio::test]
async fn img26_rejections_send_no_http() {
    let _outcomes = runtrell::test_outcomes::guard();
    let (url, http) = server(200, "", image_response()).await;
    let cfg = img26_config(url);
    let limits = cfg.image_limits.clone();
    let (mut tool, mut ctx) = fixture(cfg, ImageAction::SvgGenerate).await;
    tool.resolver.limits = limits;
    for extra in [
        json!({"stream":true}),
        json!({"partial_images":1}),
        json!({"output_compression":80}),
        json!({"prompt_rewriter":false}),
        json!({"n":2}),
        json!({"n":1}),
        json!({"output_format":"png"}),
        json!({"quality":"xhigh"}),
        json!({"quality":"max"}),
        json!({"complexity":"auto"}),
        json!({"unexpected":true}),
        json!({"markup":{}}),
        json!({"mask":{},"markup":{}}),
        json!({"references":[]}),
        json!({"svg":{"model":"no-access"}}),
        json!({"svg":{"reasoning":{"effort":"max"}}}),
        json!({"svg":{"unknown":1}}),
        json!({"svg":{"reasoning":{"effort":"low","unknown":1}}}),
    ] {
        let mut input = json!({"prompt":"draw"});
        input
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(
            tool.invoke(&ctx, input.clone()).await.is_err(),
            "accepted {input}"
        );
    }
    tool.config.action = ImageAction::Generate;
    ctx.permission = tool.descriptor().permission;
    for extra in [
        json!({"output_format":"svg"}),
        json!({"output_format":"jpeg","background":"transparent"}),
        json!({"quality":"xhigh"}),
        json!({"quality":"max"}),
        json!({"markup":{}}),
        json!({"mask":{},"markup":{}}),
    ] {
        let mut input = json!({"prompt":"draw"});
        input
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(tool.invoke(&ctx, input).await.is_err());
    }
    let staged = ArtifactStore::stage_named(
        &tool.resolver.store,
        &tool.output_access,
        ArtifactSpec {
            id: "svg-reference".into(),
            media_type: "image/svg+xml".into(),
            class: DataClass::Intermediate,
        },
        SVG_SOURCE.as_bytes(),
        0,
    )
    .await
    .unwrap();
    let reference = ArtifactStore::save(&tool.resolver.store, &tool.output_access, staged, 0)
        .await
        .unwrap();
    tool.config.action = ImageAction::Edit;
    ctx.permission = tool.descriptor().permission;
    for input in [
        json!({"prompt":"edit","references":[ImageReference::Artifact(reference.clone())]}),
        json!({"prompt":"edit","output_format":"svg","references":[ImageReference::Artifact(reference)]}),
        json!({"prompt":"edit","mask":{},"markup":{}}),
    ] {
        assert!(tool.invoke(&ctx, input).await.is_err());
    }
    assert!(http.await.unwrap().is_empty());
}

#[tokio::test]
async fn img26_svg_capability_and_author_selection_are_checked_before_http() {
    let _outcomes = runtrell::test_outcomes::guard();
    for (generation, selection, input) in [
        (false, false, json!({"prompt":"draw"})),
        (
            true,
            false,
            json!({"prompt":"draw","svg":{"model":"author","reasoning":{"effort":"low"}}}),
        ),
        (
            true,
            false,
            json!({"prompt":"draw","svg":{"reasoning":{"effort":"low"}}}),
        ),
        (
            true,
            true,
            json!({"prompt":"draw","svg":{"reasoning":{"effort":"low"}}}),
        ),
        (true, true, json!({"prompt":"draw","svg":{"model":""}})),
        (
            true,
            true,
            json!({"prompt":"draw","svg":{"model":"author","reasoning":{"effort":"invalid"}}}),
        ),
    ] {
        let (url, http) = server(200, "", image_response()).await;
        let mut cfg = img26_config(url);
        cfg.capabilities.images.svg_generation = generation;
        cfg.capabilities.images.svg_model_selection = selection;
        let limits = cfg.image_limits.clone();
        let (mut tool, ctx) = fixture(cfg, ImageAction::SvgGenerate).await;
        tool.resolver.limits = limits;
        assert!(tool.invoke(&ctx, input).await.is_err());
        assert!(http.await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn img26_raster_edit_options_and_omission_snapshot() {
    let _outcomes = runtrell::test_outcomes::guard();
    for options in [
        json!({}),
        json!({"quality":"high","complexity":"low","background":"opaque"}),
    ] {
        let (url, http) = server(200, "", image_response()).await;
        let (tool, ctx) = fixture(img26_config(url), ImageAction::Edit).await;
        let staged = ArtifactStore::stage_named(
            &tool.resolver.store,
            &tool.output_access,
            ArtifactSpec {
                id: "raster-reference".into(),
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
        let mut input = json!({"prompt":"edit","references":[ImageReference::Artifact(reference)]});
        input
            .as_object_mut()
            .unwrap()
            .extend(options.as_object().unwrap().clone());
        assert!(matches!(
            tool.invoke(&ctx, input).await.unwrap(),
            ExternalResult::Applied { .. }
        ));
        let sent = http.await.unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].starts_with("POST /v1/images/edits "));
        let wire: Value = serde_json::from_str(sent[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let mut expected = json!({"model":"gpt-image-2.6-sunburst","prompt":"edit","n":1,"output_format":"png","images":[{"image_url":format!("data:image/png;base64,{}",STANDARD.encode(png()))}]});
        expected
            .as_object_mut()
            .unwrap()
            .extend(options.as_object().unwrap().clone());
        assert_eq!(wire, expected);
    }
}

#[tokio::test]
async fn pg_img26_author_cost_settles_once_and_unknown_author_recovers_without_send() {
    let _outcomes = runtrell::test_outcomes::guard();
    use runtrell_postgres::PgStore;
    use runtrell_testkit::FakeModel;
    let database =
        std::env::var("RUNTRELL_TEST_DATABASE_URL").expect("Images 2.6 Pg acceptance never skips");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&database)
        .await
        .unwrap();
    for case in ["known", "unknown", "invalid-svg", "http-failure"] {
        let tick = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let schema = format!("img26_{}_{}", case.replace('-', "_"), tick.as_nanos());
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
            .start(RunSpec::new("tenant", case, account.clone(), "svg"))
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
        body["data"][0]["b64_json"] = json!(STANDARD.encode(if case == "invalid-svg" {
            "not XML"
        } else {
            SVG_SOURCE
        }));
        body["svg_usage"] = json!({"model":"fixture-author","input_tokens":7,"output_tokens":11,"reasoning_tokens":3,"total_tokens":18});
        if case == "http-failure" {
            body["error"] = json!({"code":"insufficient_quota","message":"redact"});
        }
        let (url, http) = server(
            if case == "http-failure" { 429 } else { 200 },
            "",
            body.clone(),
        )
        .await;
        let mut cfg = img26_config(url);
        cfg.run = handle.clone();
        let author_price = runtrell::SvgAuthorPrice {
            model: "fixture-author".into(),
            input: 2_000_000,
            output: 8_000_000,
            reasoning_in_output: true,
        };
        if case != "unknown" {
            cfg.capabilities
                .images
                .svg_author_prices
                .push(author_price.clone());
        }
        let image_price = cfg.capabilities.limits.price.clone().unwrap();
        let limits = cfg.image_limits.clone();
        let clock = TestClock::default();
        clock.set(tick.as_millis() as u64);
        let rt = Runtime {
            store: runs.clone(),
            model: FakeModel::scripted(vec![
                FakeModel::calls(vec![ToolCall {
                    id: "svg".into(),
                    name: "svg.generate".into(),
                    arguments: json!({"prompt":"draw"}),
                }]),
                FakeModel::decision(json!({"decision":"answered"})),
            ]),
            tools: vec![ImageTool {
                provider: Images::new(cfg).unwrap(),
                resolver: ImageResolver {
                    store: DataStore::memory(StoragePolicy::default()),
                    policy: FakePolicy::default(),
                    accesses: vec![access.clone()],
                    limits,
                },
                runs: runs.clone(),
                clock: clock.clone(),
                output_access: access,
                config: ImageToolConfig {
                    action: ImageAction::SvgGenerate,
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
        let op = operations
            .iter()
            .find(|o| o.request.kind == OperationKind::Tool)
            .unwrap();
        assert_eq!(op.send_count, 1);
        let before = runs.account(&account).await.unwrap();
        if case == "unknown" {
            assert!(matches!(run.state, RunState::Waiting(_)));
            assert!(!op.settled);
            assert_eq!(before.reserved, Amount::new(2048, 2048));
            let _ = rt.drive(&claim).await.unwrap();
            assert_eq!(runs.account(&account).await.unwrap(), before);
            let Some(ExternalResult::Applied { usage, .. }) = &op.result else {
                panic!("saved SVG result expected")
            };
            let mut recovered = usage.clone();
            recovered.micro_usd = None;
            let svg = recovered.svg_usage.as_mut().unwrap();
            svg.price = Some(author_price.clone());
            svg.micro_usd = Some(author_price.cost(svg).unwrap());
            svg.unknown_reason = None;
            let recovery = SettlementRecovery {
                basis: "fixture-verified-author-price".into(),
                usage: recovered,
                price: Some(image_price),
            };
            let command = OwnerCommand {
                key: "settle-author".into(),
                request_digest: digest(&json!(recovery)),
            };
            let receipt = runs
                .settle_recovered(
                    &handle,
                    &op.id,
                    recovery.clone(),
                    command.clone(),
                    tick.as_millis() as u64,
                )
                .await
                .unwrap();
            let OwnerResult::Settled(record) = &receipt.result else {
                panic!("settlement receipt expected")
            };
            assert_eq!(record.amount, Amount::new(26, 110));
            assert_eq!(
                runs.settle_recovered(&handle, &op.id, recovery, command, tick.as_millis() as u64)
                    .await
                    .unwrap(),
                receipt
            );
            let balance = runs.account(&account).await.unwrap();
            assert_eq!(balance.reserved, Amount::default());
            assert_eq!(balance.used, Amount::new(30, 114)); // first model 4 + both components.
            runtrell::test_outcomes::expect_operator_wait(&handle);
        } else {
            assert!(op.settled);
            let usage = match op.result.as_ref().unwrap() {
                ExternalResult::Applied { usage, .. } | ExternalResult::Failed { usage, .. } => {
                    usage
                }
                _ => panic!("known result"),
            };
            assert_eq!(
                usage.svg_usage.as_ref().unwrap().reported,
                body["svg_usage"]
            );
            assert_eq!(usage.amount().unwrap(), Amount::new(26, 110));
            assert_eq!(before.reserved, Amount::default());
            assert_eq!(
                before.used.micro_usd,
                if case == "known" { 118 } else { 114 }
            );
        }
        let settled: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {schema}.attempts WHERE settled AND id = $1"
        ))
        .bind(&op.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(settled, 1);
        assert_eq!(http.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn managed_img26_estimates_and_settles_bounded_raster_requests() {
    let _outcomes = runtrell::test_outcomes::guard();
    use runtrell::test_outcomes::{ExpectedOutcome, OutcomeException, expect};
    use runtrell_testkit::FakeModel;
    for (id, action, cache, expected) in [
        ("openai/gpt-image-2.6-flare", ImageAction::Generate, Some(0), Some(155)),
        ("openai/gpt-image-2.6-sunburst", ImageAction::Edit, Some(0), Some(171)),
        ("openai/gpt-image-2.6-flare", ImageAction::Generate, None, Some(155)),
        ("openai/gpt-image-2.6-flare", ImageAction::Edit, None, Some(171)),
        ("openai/gpt-image-2.6-sunburst", ImageAction::Generate, None, Some(155)),
        ("openai/gpt-image-2.6-sunburst", ImageAction::Edit, None, Some(171)),
        ("openai/gpt-image-2.6-sunburst", ImageAction::Edit, Some(1), None),
    ] {
        let edit = action == ImageAction::Edit;
        let mut response = image_response();
        if !edit {
            response["usage"]["input_tokens"] = json!(1);
            response["usage"]["input_tokens_details"]["image_tokens"] = json!(0);
        }
        if let Some(cache) = cache {
            response["usage"]["input_tokens_details"]["cached_tokens"] = json!(cache);
        }
        let (url, http) = server(200, "", response).await;
        let runs = Arc::new(MemoryStore::default());
        let account = runs.create_account("tenant", Amount::new(100_000, 300_000), None).await.unwrap();
        let run = runs.start(RunSpec::new("tenant", "raster", account.clone(), "draw")).await.unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
        let claim = runs.claim(&run, "worker", now, 100_000).await.unwrap();
        let access = Access::owner(Namespace { owner: run.clone(), agent: "main".into() });
        let clock = TestClock::default();
        clock.set(now);
        let mut provider = Images::connect(id, if edit { models::OperationKind::RasterEdit } else { models::OperationKind::RasterGenerate }, None, Connection {
            run: run.clone(), session: None, credentials: Arc::new(|_| Ok(ApiKey::new("FIXTURE-KEY"))),
            attachment_resolver: None, retry_policy: RetryPolicy::default(), timeout_ms: 10000,
            context_cap: None, input_token_cap: None, max_output_tokens: None, reasoning: None,
        }).unwrap();
        provider.set_fixture_endpoint(url);
        let artifacts = DataStore::memory(StoragePolicy::default());
        let mut arguments = json!({"prompt":"draw", "size":"1536x1024", "quality":"medium"});
        if edit {
            let staged = ArtifactStore::stage_named(&artifacts, &access, ArtifactSpec {
                id: "reference.png".into(), media_type: "image/png".into(), class: DataClass::Intermediate,
            }, &png(), now).await.unwrap();
            let image = ArtifactStore::save(&artifacts, &access, staged, now).await.unwrap();
            arguments["references"] = json!([ImageReference::Artifact(image)]);
        }
        let tool = ImageTool {
            provider, resolver: ImageResolver { store: artifacts, policy: FakePolicy::default(), accesses: vec![access.clone()], limits: ImageLimits::default() },
            runs: runs.clone(), clock: clock.clone(), output_access: access,
            config: ImageToolConfig { action, reserve: Amount::default(), timeout: 10_000 },
        };
        for (key, value) in [("quality", json!("auto")), ("size", Value::Null), ("prompt", json!("a".repeat(4097)))] {
            let mut invalid = arguments.clone();
            invalid[key] = value;
            assert!(tool.quote(&runs.get(&run).await.unwrap(), &invalid).await.is_err(), "accepted {key}");
        }
        let name = tool.descriptor().name;
        let runtime = Runtime {
            store: runs.clone(), tools: vec![tool], policy: FakePolicy::default(), clock,
            model: FakeModel::scripted(vec![FakeModel::calls(vec![ToolCall { id: "raster".into(), name, arguments }]), FakeModel::decision(json!({"decision":"answered"}))]),
            validator: Arc::new(DecisionValidator::default()), finalizer: Arc::new(DecisionFinalizer),
        };
        runtime.drive(&claim).await.unwrap();
        let operations = runs.list_operations(&run).await.unwrap();
        let op = operations.iter().find(|op| op.request.kind == OperationKind::Tool).unwrap();
        let expected_quote = Amount::new(8192 + 2264 + if edit { 14048 } else { 0 }, 8192 * 5 + 2264 * 30 + if edit { 14048 * 8 } else { 0 });
        assert_eq!(op.request.reserve, expected_quote);
        assert!(op.request.pricing.is_some());
        assert_eq!(op.send_count, 1);
        let requests = http.await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_str(requests[0].split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["quality"], "medium");
        assert_eq!(body["size"], "1536x1024");
        assert_eq!(body["n"], 1);
        assert_eq!(body["images"].as_array().map_or(0, Vec::len), usize::from(edit));
        let usage = match op.result.as_ref().unwrap() { ExternalResult::Applied { usage, .. } => usage, _ => panic!("saved image expected") };
        assert_eq!(usage.micro_usd, expected);
        assert_eq!(op.settled, expected.is_some());
        let totals = runs.account(&account).await.unwrap();
        assert_eq!(totals.reserved, if expected.is_some() { Amount::default() } else { expected_quote });
        if expected.is_none() {
            expect(
                &run,
                ExpectedOutcome {
                    exceptions: vec![OutcomeException {
                        operator_action: true,
                        ..OutcomeException::unresolved_usage(
                            "tool:1:raster",
                            OperationKind::Tool,
                            OperationState::Applied,
                            UsageIssue::PricingUnavailable,
                        )
                    }],
                    operator_wait: true,
                    ..Default::default()
                },
            );
        }
    }
}
