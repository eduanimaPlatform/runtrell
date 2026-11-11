// SPDX-License-Identifier: Apache-2.0
//! OpenAI Images API usage normalization. Fixtures follow the published `ImagesUsage`
//! schema (openai/openai-openapi main c2aecc2, read 2026-11-03) and the stored usage of
//! a real image operation whose cost stayed unknown.
#![cfg(feature = "openai")]
use runtrell::{Error, ModelLimits, Operation, Price, RunHandle, SettlementRecovery, Usage};
use runtrell_providers::{
    ApiKey, Capabilities, Config, CounterSupport, DocumentCapabilities, DocumentLimits,
    ImageCapabilities, ImageLimits,
    openai_images_usage::{self as images_usage, normalize},
    recovery_for_stored_openai_images_usage,
};
use serde_json::{Value, json};

#[test]
fn img26_synthetic_usage_prices_missing_cache_as_zero_and_recovers_stored_usage() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/images26-synthetic-usage.json")).unwrap();
    assert!(
        fixture["note"]
            .as_str()
            .unwrap()
            .starts_with("Synthetic provider response")
    );
    for model in ["gpt-image-2.6-sunburst", "gpt-image-2.6-flare"] {
        let cfg = config(model, Some(price(model)));
        let missing = normalize(&cfg, &fixture);
        assert_eq!(
            (missing.cached, missing.cached_image_input),
            (Some(0), Some(0))
        );
        assert_eq!(missing.micro_usd, Some(1_870));
        let stored = normalize(&config(model, None), &fixture);
        assert_eq!(
            (stored.cached, stored.cached_image_input),
            (Some(0), Some(0))
        );
        assert_eq!(stored.micro_usd, None);
        let recovered = recovery_for_stored_openai_images_usage(&stored, &price(model)).unwrap();
        assert_eq!(recovered.basis, images_usage::BASIS);
        assert_eq!(recovered.price.cost(&recovered.usage), Ok(1_870));
        let mut old_stored = stored.clone();
        old_stored.cached = None;
        old_stored.cached_image_input = None;
        let recovered =
            recovery_for_stored_openai_images_usage(&old_stored, &price(model)).unwrap();
        assert_eq!(
            (recovered.usage.cached, recovered.usage.cached_image_input),
            (Some(0), Some(0))
        );
        assert_eq!(recovered.price.cost(&recovered.usage), Ok(1_870));
        let flat_price = Price {
            cached: 5_000_000,
            cached_image_input: 8_000_000,
            ..price(model)
        };
        assert!(recovery_for_stored_openai_images_usage(&old_stored, &flat_price).is_ok());

        for (cached, cached_image) in [(0, 0), (12, 8)] {
            let mut response = fixture.clone();
            response["usage"]["input_tokens_details"]["cached_tokens"] = json!(cached);
            response["usage"]["input_tokens_details"]["cached_tokens_details"] =
                json!({"image_tokens":cached_image});
            let measured = normalize(&cfg, &response);
            assert_eq!(
                (measured.cached, measured.cached_image_input),
                (Some(cached), Some(cached_image))
            );
            if cached == 0 {
                assert_eq!(measured.micro_usd, Some(1_870));
            } else {
                assert_eq!(measured.micro_usd, None);
                assert_eq!(
                    measured.unknown_reason.as_deref(),
                    Some(images_usage::REASON_INCONSISTENT)
                );
                let reported_stored = normalize(&config(model, None), &response);
                assert!(
                    recovery_for_stored_openai_images_usage(&reported_stored, &price(model))
                        .is_err()
                );
            }
        }

        let mut partial = old_stored.clone();
        partial.cached = Some(0);
        let recovered = recovery_for_stored_openai_images_usage(&partial, &price(model)).unwrap();
        assert_eq!(
            (recovered.usage.cached, recovered.usage.cached_image_input),
            (Some(0), Some(0))
        );
        let mut invalid = old_stored.clone();
        invalid.cached = Some(3);
        invalid.cached_image_input = Some(4);
        assert!(recovery_for_stored_openai_images_usage(&invalid, &price(model)).is_err());
        let mut complete = old_stored.clone();
        complete.cached = Some(0);
        complete.cached_image_input = Some(0);
        complete.input = None;
        let incomplete_recovery =
            recovery_for_stored_openai_images_usage(&complete, &price(model)).unwrap();
        assert_eq!(
            incomplete_recovery.price.cost(&incomplete_recovery.usage),
            Err(Error::UsageUnknown)
        );
    }
    let unknown_model = "gpt-image-2.6-unlisted";
    let mut complete_unknown =
        normalize(&config(unknown_model, Some(price(unknown_model))), &fixture);
    complete_unknown.cached = Some(0);
    complete_unknown.cached_image_input = Some(0);
    assert_eq!(
        recovery_for_stored_openai_images_usage(&complete_unknown, &price(unknown_model)),
        Err(Error::UsageUnknown)
    );
}

#[test]
fn img26_standard_catalog_prices_direct_images_with_zero_cache() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/images26-standard.json")).unwrap();
    assert_eq!(fixture["tier"], "standard");
    assert_eq!(fixture["accessed"], "2026-11-05");
    for entry in fixture["prices"].as_array().unwrap() {
        let price: Price = serde_json::from_value(entry.clone()).unwrap();
        assert_eq!(
            (
                price.input,
                price.cached,
                price.image_input,
                price.cached_image_input,
                price.image_output
            ),
            (5_000_000, 1_250_000, 8_000_000, 2_000_000, 30_000_000)
        );
        let cfg = config(&price.model, Some(price.clone()));
        let mut response = official_minimal();
        let normalized = normalize(&cfg, &response);
        assert_eq!(
            (
                normalized.cached,
                normalized.cached_image_input,
                normalized.micro_usd
            ),
            (Some(0), Some(0), Some(1_870))
        );
        response["usage"]["input_tokens_details"]["cached_tokens"] = json!(12);
        response["usage"]["input_tokens_details"]["cached_tokens_details"] =
            json!({"image_tokens":8});
        let reported = normalize(&cfg, &response);
        assert_eq!(
            (reported.cached, reported.cached_image_input),
            (Some(12), Some(8))
        );
        assert_eq!(reported.micro_usd, None);
        assert_eq!(
            reported.unknown_reason.as_deref(),
            Some(images_usage::REASON_INCONSISTENT)
        );
    }
}

#[test]
fn img26_svg_usage_preserves_raw_and_keeps_missing_malformed_and_unpriced_unknown() {
    let cfg = sunburst();
    for raw in [
        Value::Null,
        json!("malformed"),
        json!({"model":12}),
        json!({"model":"unlisted","input_tokens":"7","output_tokens":11}),
        json!({"model":"unlisted","input_tokens":7,"output_tokens":11,"total_tokens":99}),
        json!({"model":"unlisted","input_tokens":7,"output_tokens":11,"reasoning_tokens":3,"total_tokens":18,"extra":"verbatim"}),
        json!({"model":"unlisted","input_tokens":7}),
    ] {
        let mut response = official_minimal();
        response["svg_usage"] = raw.clone();
        let usage = images_usage::normalize_with_svg(&cfg, &response, true);
        let svg = usage.svg_usage.as_ref().unwrap();
        assert_eq!(svg.reported, raw);
        assert_eq!(svg.micro_usd, None);
        assert!(svg.unknown_reason.is_some());
        assert_eq!(usage.amount(), Err(Error::UsageUnknown));
        assert_eq!(
            (
                usage.input,
                usage.output,
                usage.image_input,
                usage.image_output
            ),
            (Some(50), Some(50), Some(40), Some(50))
        );
        let stored: Usage = serde_json::from_value(serde_json::to_value(&usage).unwrap()).unwrap();
        assert_eq!(stored, usage);
    }
    let absent = images_usage::normalize_with_svg(&cfg, &official_minimal(), true);
    assert_eq!(
        absent.svg_usage.unwrap().unknown_reason.as_deref(),
        Some("SVG_USAGE_ABSENT")
    );
    assert!(normalize(&cfg, &official_minimal()).svg_usage.is_none());
}

#[test]
fn img26_svg_exact_author_price_is_separate_and_totals_are_not_double_counted() {
    let mut cfg = sunburst();
    cfg.capabilities.images.svg_author_prices = vec![runtrell::SvgAuthorPrice {
        model: "author-v1".into(),
        input: 2_000_000,
        output: 8_000_000,
        reasoning_in_output: true,
    }];
    let mut response = official_minimal();
    response["svg_usage"] = json!({"model":"author-v1","input_tokens":7,"output_tokens":11,"reasoning_tokens":3,"total_tokens":18});
    let usage = normalize(&cfg, &response);
    assert_eq!(usage.svg_usage.as_ref().unwrap().micro_usd, Some(102));
    assert_eq!(usage.total().unwrap(), 118); // image 100 + author 18, never +total/+reasoning.
    assert_eq!(
        usage.amount().unwrap().micro_usd,
        usage.micro_usd.unwrap() + 102
    );
    let image_cost = cfg
        .capabilities
        .limits
        .price
        .as_ref()
        .unwrap()
        .cost(&usage)
        .unwrap();
    assert_eq!(Some(image_cost), usage.micro_usd);
    let mut dispatch = runtrell::DispatchRequest::new(
        "svg-test",
        runtrell::OperationKind::Tool,
        "svg.generate",
        json!({}),
        runtrell::Amount::default(),
    );
    dispatch.price = cfg.capabilities.limits.price.clone();
    assert_eq!(
        dispatch.settlement_amount(&usage).unwrap(),
        Some(usage.amount().unwrap())
    );
    cfg.capabilities.images.svg_author_prices[0].reasoning_in_output = false;
    assert_eq!(
        normalize(&cfg, &response).amount(),
        Err(Error::UsageUnknown)
    );
    cfg.capabilities.images.svg_author_prices[0].reasoning_in_output = true;
    response["svg_usage"]["model"] = json!("author-v2");
    let unknown = normalize(&cfg, &response);
    assert_eq!(
        unknown.svg_usage.as_ref().unwrap().model.as_deref(),
        Some("author-v2")
    );
    assert_eq!(unknown.amount(), Err(Error::UsageUnknown));
    response["svg_usage"]["model"] = json!("author-v1");
    response["svg_usage"]["reasoning_tokens"] = json!(12);
    assert_eq!(
        normalize(&cfg, &response).amount(),
        Err(Error::UsageUnknown)
    );
}

#[test]
fn img26_owner_recovery_prices_author_without_losing_known_image_cost() {
    let mut cfg = sunburst();
    let mut response = official_minimal();
    response["svg_usage"] = json!({"model":"author","input_tokens":7,"output_tokens":11,"reasoning_tokens":3,"total_tokens":18});
    let stored = normalize(&cfg, &response);
    assert_eq!(stored.micro_usd, Some(1870));
    assert_eq!(stored.amount(), Err(Error::UsageUnknown));
    let op = Operation {
        id: "svg-op".into(),
        run: cfg.run.clone(),
        request: runtrell::DispatchRequest::new(
            "svg-op",
            runtrell::OperationKind::Tool,
            "svg.generate",
            json!({}),
            runtrell::Amount::new(1000, 5000),
        ),
        state: runtrell::OperationState::Applied,
        result: Some(runtrell::ExternalResult::Applied {
            physical_evidence: None,
            output: json!({}),
            usage: stored.clone(),
            external_id: None,
        }),
        settled: false,
        unknown_usage: true,
        send_generation: Some(1),
        send_count: 1,
        replay_proof_generation: None,
        created_at: 0,
        finished_at: Some(1),
        usage_observations: vec![],
        unsettled_result: None,
        recovery: None,
    };
    cfg.capabilities.images.svg_author_prices = vec![runtrell::SvgAuthorPrice {
        model: "author".into(),
        input: 2_000_000,
        output: 8_000_000,
        reasoning_in_output: true,
    }];
    let mut refined = normalize(&cfg, &response);
    refined.micro_usd = None; // owner API computes and checks the primary image cost.
    let recovery = SettlementRecovery {
        basis: "test-verified-author-rate".into(),
        usage: refined,
        price: price(SUNBURST),
    };
    let (_, record) = op.plan_recovery(&recovery, 2).unwrap().unwrap();
    assert_eq!(record.amount, runtrell::Amount::new(118, 1972));
    assert_eq!(record.previous, stored);
    let mut removed = recovery.clone();
    removed.usage.svg_usage = None;
    assert!(op.plan_recovery(&removed, 2).is_err());
    let mut rewritten = recovery;
    rewritten.usage.svg_usage.as_mut().unwrap().reported["input_tokens"] = json!(0);
    assert!(op.plan_recovery(&rewritten, 2).is_err());
}

#[test]
fn img26_malformed_author_cannot_become_free_or_priced_in_owner_recovery() {
    let cfg = sunburst();
    let mut response = official_minimal();
    response["svg_usage"] = json!({"model":"author","input_tokens":7,"output_tokens":11,"reasoning_tokens":"bad","total_tokens":18});
    let mut usage = normalize(&cfg, &response);
    let svg = usage.svg_usage.as_mut().unwrap();
    assert_eq!(svg.unknown_reason.as_deref(), Some("SVG_USAGE_MALFORMED"));
    svg.price = Some(runtrell::SvgAuthorPrice {
        model: "author".into(),
        input: 2_000_000,
        output: 8_000_000,
        reasoning_in_output: true,
    });
    svg.micro_usd = Some(102);
    svg.unknown_reason = None;
    assert_eq!(usage.amount(), Err(Error::UsageUnknown));
}

#[test]
fn img26_exact_family_cache_contradiction_stays_unknown() {
    for model in images_usage::GPT_IMAGE_2_FAMILY {
        let cfg = config(model, Some(price(model)));
        let mut response = official_minimal();
        response["usage"]["input_tokens_details"]["cached_tokens"] = json!(12);
        response["usage"]["input_tokens_details"]["cached_tokens_details"] =
            json!({"image_tokens":8});
        let usage = normalize(&cfg, &response);
        assert_eq!(
            (usage.cached, usage.cached_image_input),
            (Some(12), Some(8))
        );
        assert_eq!(
            usage.micro_usd, None,
            "discounted impossible cache on {model}"
        );
        assert_eq!(
            usage.unknown_reason.as_deref(),
            Some(images_usage::REASON_INCONSISTENT)
        );
    }
}

const SUNBURST: &str = "gpt-image-2.5-sunburst";

#[test]
fn img26_zero_cache_prices_image_and_author_evidence() {
    let mut cfg = config("gpt-image-2.6-flare", Some(price("gpt-image-2.6-flare")));
    cfg.capabilities.images.svg_author_prices = vec![runtrell::SvgAuthorPrice {
        model: "gpt-6-luna".into(),
        input: 100_000,
        output: 500_000,
        reasoning_in_output: true,
    }];
    let response = json!({"usage":{"total_tokens":935,"input_tokens":111,"output_tokens":824,
        "input_tokens_details":{"text_tokens":111,"image_tokens":0,"cached_tokens":null,
            "cached_tokens_details":{"image_tokens":null}},
        "output_tokens_details":{"image_tokens":824,"text_tokens":0}},
        "svg_usage":{"model":"gpt-6-luna","input_tokens":1583,"output_tokens":2304,
            "reasoning_tokens":1779,"total_tokens":3887}});
    let u = images_usage::normalize_with_svg(&cfg, &response, true);
    assert_eq!((u.cached, u.cached_image_input), (Some(0), Some(0)));
    assert_eq!(
        u.price_evidence.as_ref().unwrap().input_micro_usd,
        Some(555)
    );
    assert_eq!(
        u.price_evidence.as_ref().unwrap().output_micro_usd,
        Some(24_720)
    );
    assert_eq!(u.svg_usage.as_ref().unwrap().micro_usd, Some(1_311));
    assert_eq!(u.micro_usd, Some(25_275));
    assert_eq!(u.amount(), Ok(runtrell::Amount::new(4822, 26_586)));
    let stored: Usage = serde_json::from_value(serde_json::to_value(&u).unwrap()).unwrap();
    assert_eq!(stored, u);

    let mut old = u.clone();
    old.cached = None;
    old.cached_image_input = None;
    old.micro_usd = None;
    old.unknown_reason = Some(images_usage::REASON_CACHED_UNREPORTED.into());
    old.price_evidence = None; // owner derives evidence from the verified price
    let op = Operation {
        id: "img26".into(),
        run: cfg.run.clone(),
        request: runtrell::DispatchRequest::new(
            "img26",
            runtrell::OperationKind::Tool,
            "svg.generate",
            json!({}),
            runtrell::Amount::new(5000, 50000),
        ),
        state: runtrell::OperationState::Applied,
        result: Some(runtrell::ExternalResult::Applied {
            physical_evidence: None,
            output: json!({}),
            usage: old.clone(),
            external_id: None,
        }),
        settled: false,
        unknown_usage: true,
        send_generation: Some(1),
        send_count: 1,
        replay_proof_generation: None,
        created_at: 0,
        finished_at: Some(1),
        usage_observations: vec![],
        unsettled_result: None,
        recovery: None,
    };
    let recovery =
        recovery_for_stored_openai_images_usage(&old, &price("gpt-image-2.6-flare")).unwrap();
    let (_, record) = op.plan_recovery(&recovery, 2).unwrap().unwrap();
    assert_eq!(record.previous, old);
    assert_eq!(record.amount, runtrell::Amount::new(4822, 26_586));
    assert_eq!(
        record
            .settled
            .price_evidence
            .as_ref()
            .unwrap()
            .output_micro_usd,
        Some(24_720)
    );
}

#[test]
fn incomplete_images_usage_retains_only_consistent_known_price_components() {
    let cfg = config("gpt-image-2.6-flare", Some(price("gpt-image-2.6-flare")));
    let mut response = json!({"usage":{"input_tokens":111,"output_tokens":824,
        "input_tokens_details":{"text_tokens":111,"image_tokens":0,
            "cached_tokens":0,"cached_tokens_details":{"image_tokens":0}},
        "output_tokens_details":{"image_tokens":824,"text_tokens":0}}});
    response["usage"]
        .as_object_mut()
        .unwrap()
        .remove("input_tokens");
    let missing_input = normalize(&cfg, &response);
    assert_eq!(
        missing_input.unknown_reason.as_deref(),
        Some(images_usage::REASON_INCOMPLETE)
    );
    assert_eq!(missing_input.micro_usd, None);
    assert_eq!(
        missing_input
            .price_evidence
            .as_ref()
            .unwrap()
            .input_micro_usd,
        None
    );
    assert_eq!(
        missing_input
            .price_evidence
            .as_ref()
            .unwrap()
            .output_micro_usd,
        Some(24_720)
    );
    response["usage"]["output_tokens_details"]["text_tokens"] = json!(1);
    let inconsistent_output = normalize(&cfg, &response);
    assert_eq!(
        inconsistent_output.unknown_reason.as_deref(),
        Some(images_usage::REASON_INCONSISTENT)
    );
    assert!(inconsistent_output.price_evidence.is_none());

    response["usage"]["input_tokens"] = json!(111);
    response["usage"]
        .as_object_mut()
        .unwrap()
        .remove("output_tokens");
    response["usage"]["output_tokens_details"]["text_tokens"] = json!(0);
    let missing_output = normalize(&cfg, &response);
    assert_eq!(
        missing_output.unknown_reason.as_deref(),
        Some(images_usage::REASON_INCOMPLETE)
    );
    assert_eq!(missing_output.micro_usd, None);
    assert_eq!(
        missing_output
            .price_evidence
            .as_ref()
            .unwrap()
            .input_micro_usd,
        Some(555)
    );
    assert_eq!(
        missing_output
            .price_evidence
            .as_ref()
            .unwrap()
            .output_micro_usd,
        None
    );
    response["usage"]["input_tokens_details"]["image_tokens"] = json!(1);
    let inconsistent_input = normalize(&cfg, &response);
    assert_eq!(
        inconsistent_input.unknown_reason.as_deref(),
        Some(images_usage::REASON_INCONSISTENT)
    );
    assert!(inconsistent_input.price_evidence.is_none());
    response["usage"]["output_tokens"] = json!(824);
    response["usage"]["input_tokens_details"]["text_tokens"] = json!(200);
    response["usage"]["input_tokens_details"]
        .as_object_mut()
        .unwrap()
        .remove("image_tokens");
    let oversized_text = normalize(&cfg, &response);
    assert_eq!(
        oversized_text.unknown_reason.as_deref(),
        Some(images_usage::REASON_INCONSISTENT)
    );
    assert!(oversized_text.price_evidence.is_none());

    response["usage"]
        .as_object_mut()
        .unwrap()
        .remove("input_tokens");
    response["usage"]["total_tokens"] = json!(100);
    let total_below_output = normalize(&cfg, &response);
    assert_eq!(
        total_below_output.unknown_reason.as_deref(),
        Some(images_usage::REASON_INCONSISTENT)
    );
    assert!(total_below_output.price_evidence.is_none());
}

/// GPT Image 2.5 Standard rates (USD per million tokens): text 5 / cached 1.25,
/// image 8 / cached 2, image output 30; integer micro-USD per million tokens.
fn price(model: &str) -> Price {
    Price {
        model: model.into(),
        input: 5_000_000,
        cached: 1_250_000,
        output: 30_000_000,
        reasoning: 30_000_000,
        image_input: 8_000_000,
        image_output: 30_000_000,
        cached_image_input: 2_000_000,
    }
}
fn config(model: &str, price: Option<Price>) -> Config {
    Config {
        endpoint: "https://api.example.test/v1/images/generations".into(),
        key: ApiKey::new("TEST-KEY-DO-NOT-PRINT"),
        capabilities: Capabilities {
            limits: ModelLimits {
                model: model.into(),
                context: 8192,
                max_output: 4096,
                min_max_output_tokens: 1,
                price,
                timeout: 30_000,
            },
            json_schema: false,
            tool_calling: false,
            parallel_tool_calls: false,
            images: ImageCapabilities {
                generation: true,
                ..Default::default()
            },
            documents: DocumentCapabilities::default(),
            counters: CounterSupport {
                input: true,
                cached: false,
                output: true,
                reasoning: false,
                image_input: true,
                image_output: true,
                cached_image_input: false,
            },
        },
        run: RunHandle {
            tenant: "t".into(),
            id: "r".into(),
        },
        session: None,
        input_token_upper_bound: 100,
        max_request_bytes: 65536,
        max_response_bytes: 65536,
        image_limits: ImageLimits::default(),
        document_limits: DocumentLimits::default(),
        attachment_resolver: None,
    }
}
fn sunburst() -> Config {
    config(SUNBURST, Some(price(SUNBURST)))
}
/// The documented minimal example response of `POST /images/generations`.
fn official_minimal() -> Value {
    json!({"created":1713833628,"data":[{"b64_json":"..."}],
        "usage":{"total_tokens":100,"input_tokens":50,"output_tokens":50,
                 "input_tokens_details":{"text_tokens":10,"image_tokens":40}}})
}
/// Counters stored for the real image operation that stayed unsettled: text prompt
/// only, one 601-token image, no cached counter in the response.
fn stored_response() -> Value {
    json!({"data":[{"b64_json":"..."}],
        "usage":{"total_tokens":944,"input_tokens":343,"output_tokens":601,
                 "input_tokens_details":{"text_tokens":343,"image_tokens":0},
                 "output_tokens_details":{"image_tokens":601,"text_tokens":0}}})
}

#[test]
fn official_minimal_usage_is_priced_with_cached_zero_for_gpt_image_2() {
    let u = normalize(&sunburst(), &official_minimal());
    assert_eq!(u.model, SUNBURST);
    assert_eq!(
        (u.input, u.output, u.image_input, u.image_output),
        (Some(50), Some(50), Some(40), Some(50))
    );
    // The spec has no cached counter, and the guide says cached pricing does not apply to
    // direct Images API requests for GPT Image 2 / 2.5: exactly 0, not unknown.
    assert_eq!((u.cached, u.cached_image_input), (Some(0), Some(0)));
    assert_eq!(u.quantities["text_input_tokens"], 10);
    // 10 text @5 + 40 image @8 + 50 image output @30 = 50 + 320 + 1500 micro-USD.
    assert_eq!(u.micro_usd, Some(1_870));
    assert_eq!(u.unknown_reason, None);
    assert_eq!(u.total().unwrap(), 100);
    assert_eq!(u.amount().unwrap().micro_usd, 1_870);
}

#[test]
fn stored_operation_shape_now_settles_to_the_exact_amount() {
    let u = normalize(&sunburst(), &stored_response());
    assert_eq!((u.input, u.output), (Some(343), Some(601)));
    assert_eq!((u.image_input, u.image_output), (Some(0), Some(601)));
    assert_eq!(u.quantities["text_input_tokens"], 343);
    assert_eq!(u.quantities["text_output_tokens"], 0);
    // ceil((343 * 5_000_000 + 601 * 30_000_000) / 1_000_000)
    assert_eq!(u.micro_usd, Some(19_745));
}

#[test]
fn the_responses_path_could_not_price_the_same_body() {
    // Before the Images-specific path: cached stayed None, so a price whose cached rate
    // differs from the input rate refused the cost. Price::cost keeps that refusal.
    let mut raw = Usage {
        model: SUNBURST.into(),
        input: Some(343),
        output: Some(601),
        image_input: Some(0),
        image_output: Some(601),
        reasoning: Some(0),
        ..Usage::default()
    };
    assert_eq!(price(SUNBURST).cost(&raw), Err(Error::UsageUnknown));
    raw.cached = Some(0);
    raw.cached_image_input = Some(0);
    assert_eq!(price(SUNBURST).cost(&raw), Ok(19_745));
}

#[test]
fn models_outside_the_documented_family_keep_cached_unknown_with_a_distinct_reason() {
    for model in [
        "gpt-image-1",
        "gpt-image-1.5",
        "gpt-image-1-mini",
        "gpt-image-20",
    ] {
        let u = normalize(&config(model, Some(price(model))), &official_minimal());
        assert_eq!((u.cached, u.cached_image_input), (None, None), "{model}");
        assert_eq!(u.micro_usd, None, "{model}");
        assert_eq!(
            u.unknown_reason.as_deref(),
            Some(images_usage::REASON_CACHED_UNREPORTED),
            "{model}"
        );
        // Counters the spec does give are still reported.
        assert_eq!((u.input, u.image_output), (Some(50), Some(50)));
    }
    // A host price that cannot be affected by cached counters still prices.
    let flat = Price {
        cached: 5_000_000,
        cached_image_input: 8_000_000,
        ..price("gpt-image-1")
    };
    let u = normalize(&config("gpt-image-1", Some(flat)), &official_minimal());
    assert_eq!(u.micro_usd, Some(1_870));
}

#[test]
fn family_matcher_is_exact() {
    use images_usage::cached_input_never_applies as zero;
    for yes in images_usage::GPT_IMAGE_2_FAMILY {
        assert!(zero(yes), "{yes}");
    }
    // Not in the documented list: older models, an unreleased future model, near-misses,
    // the Responses image tool and non-image models. None is inferred from a prefix.
    for no in [
        "gpt-image-1",
        "gpt-image-1.5",
        "gpt-image-1-mini",
        "chatgpt-image-latest",
        "gpt-image-2.6",
        "gpt-image-3",
        "gpt-image-2.5",
        "gpt-image-2.5-sunburst-2099-01-01",
        "gpt-image-2.6-sunburst-2026-11-11",
        "gpt-image-2.6-flare-2026-11-11",
        "gpt-image-2-mini",
        "gpt-image-20",
        "gpt-image-2x",
        "gpt-image-2.",
        "image-gpt-image-2",
        "GPT-IMAGE-2",
        "image_generation",
        "gpt-6-astra",
        "gpt-5.5",
        "dall-e-3",
        "",
    ] {
        assert!(!zero(no), "{no}");
    }
}

#[test]
fn model_alias_unknown_model_and_missing_price() {
    // Reported model equals the requested one, or the field is absent: priced.
    let mut body = official_minimal();
    body["model"] = json!(SUNBURST);
    assert_eq!(normalize(&sunburst(), &body).micro_usd, Some(1_870));
    // A snapshot name is priced only when the host declared it an alias; the usage is
    // then recorded under the priced model so settlement's price/model check holds.
    body["model"] = json!("gpt-image-2.5-sunburst-2026-09-08");
    let unknown = normalize(&sunburst(), &body);
    assert_eq!(unknown.micro_usd, None);
    assert_eq!(unknown.model, "gpt-image-2.5-sunburst-2026-09-08");
    assert_eq!(
        unknown.unknown_reason.as_deref(),
        Some(images_usage::REASON_MODEL_UNPRICED)
    );
    let mut aliased = sunburst();
    aliased.capabilities.images.response_model_aliases =
        vec!["gpt-image-2.5-sunburst-2026-09-08".into()];
    let u = normalize(&aliased, &body);
    assert_eq!((u.model.as_str(), u.micro_usd), (SUNBURST, Some(1_870)));
    // An undeclared different model is never priced with the configured model's rates.
    body["model"] = json!("gpt-image-2.5-flare");
    assert_eq!(normalize(&aliased, &body).micro_usd, None);
    // No host price: counters are reported, cost stays unknown with its own reason.
    let u = normalize(&config(SUNBURST, None), &official_minimal());
    assert_eq!(u.micro_usd, None);
    assert_eq!(
        u.unknown_reason.as_deref(),
        Some(images_usage::REASON_NO_PRICE)
    );
    assert_eq!(u.input, Some(50));
}

#[test]
fn absent_malformed_incomplete_or_inconsistent_usage_is_never_filled_in() {
    let cases = [
        (json!({"data":[]}), images_usage::REASON_ABSENT),
        (json!({"usage":null}), images_usage::REASON_ABSENT),
        (
            json!({"usage":{"input_tokens":"3","output_tokens":5,
                "input_tokens_details":{"text_tokens":1,"image_tokens":2}}}),
            images_usage::REASON_MALFORMED,
        ),
        (
            json!({"usage":{"input_tokens":-3,"output_tokens":5,
                "input_tokens_details":{"text_tokens":1,"image_tokens":2}}}),
            images_usage::REASON_MALFORMED,
        ),
        (
            json!({"usage":{"input_tokens":3,"output_tokens":5}}),
            images_usage::REASON_INCOMPLETE,
        ),
        (
            json!({"usage":{"input_tokens":3,"input_tokens_details":{"text_tokens":1,"image_tokens":2}}}),
            images_usage::REASON_INCOMPLETE,
        ),
        // text + image != input
        (
            json!({"usage":{"input_tokens":9,"output_tokens":5,
                "input_tokens_details":{"text_tokens":1,"image_tokens":2}}}),
            images_usage::REASON_INCONSISTENT,
        ),
        // total != input + output
        (
            json!({"usage":{"total_tokens":9,"input_tokens":3,"output_tokens":5,
                "input_tokens_details":{"text_tokens":1,"image_tokens":2}}}),
            images_usage::REASON_INCONSISTENT,
        ),
        // output details do not add up to output
        (
            json!({"usage":{"input_tokens":3,"output_tokens":5,
                "input_tokens_details":{"text_tokens":1,"image_tokens":2},
                "output_tokens_details":{"image_tokens":4,"text_tokens":0}}}),
            images_usage::REASON_INCONSISTENT,
        ),
        // u64 overflow in text + image
        (
            json!({"usage":{"input_tokens":3,"output_tokens":5,
                "input_tokens_details":{"text_tokens":u64::MAX,"image_tokens":4}}}),
            images_usage::REASON_INCONSISTENT,
        ),
    ];
    for (body, reason) in cases {
        let u = normalize(&sunburst(), &body);
        assert_eq!(u.unknown_reason.as_deref(), Some(reason), "{body}");
        // Counters the response does give are reported as given; the rest stay unknown.
        if reason == images_usage::REASON_INCOMPLETE {
            assert_eq!(
                u.input,
                body.pointer("/usage/input_tokens").and_then(Value::as_u64)
            );
            assert_eq!(
                u.output,
                body.pointer("/usage/output_tokens").and_then(Value::as_u64)
            );
        } else {
            assert_eq!((u.input, u.output), (None, None), "{body}");
        }
        assert_eq!((u.cached, u.micro_usd), (None, None), "{body}");
        assert_eq!(u.model, SUNBURST);
        assert!(u.amount().is_err());
    }
}

#[test]
fn zero_token_edges_and_large_values() {
    // An image-only prompt with a cached-eligible family: zero text tokens are real zeros.
    let body = json!({"usage":{"total_tokens":1010,"input_tokens":10,"output_tokens":1000,
        "input_tokens_details":{"text_tokens":0,"image_tokens":10}}});
    let u = normalize(&sunburst(), &body);
    assert_eq!(
        (u.image_input, u.quantities["text_input_tokens"]),
        (Some(10), 0)
    );
    assert_eq!(u.micro_usd, Some(80 + 30_000));
    // Everything zero is a real zero, not unknown.
    let zero = json!({"usage":{"total_tokens":0,"input_tokens":0,"output_tokens":0,
        "input_tokens_details":{"text_tokens":0,"image_tokens":0}}});
    assert_eq!(normalize(&sunburst(), &zero).micro_usd, Some(0));
    // The largest counters that still add up are priced with checked arithmetic.
    let big = json!({"usage":{"input_tokens":u64::MAX / 2,"output_tokens":1,
        "input_tokens_details":{"text_tokens":u64::MAX / 2,"image_tokens":0}}});
    let u = normalize(&sunburst(), &big);
    assert_eq!(u.input, Some(u64::MAX / 2));
    // Cost does not fit u64 micro-USD: refused, never wrapped or clamped.
    assert_eq!(u.micro_usd, None);
    assert_eq!(
        u.unknown_reason.as_deref(),
        Some(images_usage::REASON_OVERFLOW)
    );
}

#[test]
fn a_distinct_cached_image_rate_is_priced_without_double_counting() {
    // Four rates, all different: ordinary text 5, cached text 1.25, ordinary image 8,
    // cached image 2. 1000 input = 400 text + 100 cached text + 200 image + 300... build a
    // case by hand: 400 ordinary text, 300 cached text, 200 ordinary image, 100 cached image.
    let p = price("m");
    let u = Usage {
        model: "m".into(),
        input: Some(1_000),
        output: Some(10),
        cached: Some(400),      // cached text 300 + cached image 100
        image_input: Some(300), // ordinary image 200 + cached image 100
        cached_image_input: Some(100),
        reasoning: Some(0),
        image_output: Some(10),
        ..Usage::default()
    };
    // text 400*5 + cachedtext 300*1.25 + image 200*8 + cachedimage 100*2 + out 10*30
    // = 2000 + 375 + 1600 + 200 + 300 = 4475 micro-USD.
    assert_eq!(p.cost(&u), Ok(4_475));
    // Changing only the cached image rate changes only that line.
    let p2 = Price {
        cached_image_input: 3_000_000,
        ..p.clone()
    };
    assert_eq!(p2.cost(&u), Ok(4_575));
    // Without the intersection count the split is unknown and refused.
    let unknown = Usage {
        cached_image_input: None,
        ..u.clone()
    };
    assert_eq!(p.cost(&unknown), Err(Error::UsageUnknown));
    // A cached image rate equal to the text cached rate must not hide a real difference:
    // equal rates need no intersection count, different rates do.
    let same = Price {
        cached_image_input: p.cached,
        image_input: p.input,
        ..p.clone()
    };
    assert!(same.cost(&unknown).is_ok());
}

#[test]
fn recovery_applies_the_same_spec_rule_to_stored_usage_and_refuses_other_models() {
    let stored = Usage {
        model: SUNBURST.into(),
        input: Some(343),
        output: Some(601),
        image_input: Some(0),
        image_output: Some(601),
        quantities: [("text_input_tokens".to_string(), 343)].into(),
        ..Usage::default()
    };
    let recovery = recovery_for_stored_openai_images_usage(&stored, &price(SUNBURST)).unwrap();
    assert_eq!(recovery.basis, images_usage::BASIS);
    assert_eq!(
        (
            recovery.usage.cached,
            recovery.usage.cached_image_input,
            recovery.usage.reasoning
        ),
        (Some(0), Some(0), None)
    );
    assert_eq!(price(SUNBURST).cost(&recovery.usage), Ok(19_745));
    // Counters the stored usage already had are untouched.
    assert_eq!(recovery.usage.input, stored.input);
    // A model the spec does not cover stays for the operator.
    let other = Usage {
        model: "gpt-image-1".into(),
        ..stored.clone()
    };
    assert_eq!(
        recovery_for_stored_openai_images_usage(&other, &price("gpt-image-1")),
        Err(Error::UsageUnknown)
    );
    // A stored cached count other than zero contradicts the rule: refused, not rewritten.
    let contradictory = Usage {
        cached: Some(5),
        ..stored
    };
    assert!(matches!(
        recovery_for_stored_openai_images_usage(&contradictory, &price(SUNBURST)),
        Err(Error::Invalid(_))
    ));
    let _: Option<(Operation, SettlementRecovery)> = None;
}

#[test]
fn reasoning_is_not_invented_a_price_that_depends_on_it_stays_unknown() {
    let differing = Price {
        reasoning: 40_000_000,
        ..price(SUNBURST)
    };
    let u = normalize(&config(SUNBURST, Some(differing)), &official_minimal());
    assert_eq!(u.reasoning, None);
    assert_eq!((u.cached, u.micro_usd), (Some(0), None));
    assert_eq!(
        u.unknown_reason.as_deref(),
        Some(images_usage::REASON_PRICE_COUNTER_UNKNOWN)
    );
    // Equal reasoning and output rates make the unreported count irrelevant.
    assert_eq!(
        normalize(&sunburst(), &official_minimal()).micro_usd,
        Some(1_870)
    );
}

/// Hand-computable fixtures (GPT Image 2.5 Standard rates, USD per million tokens: text 5,
/// cached text 1.25, image 8, cached image 2, image output 30; read 2026-11-04). Not live
/// provider costs.
#[test]
fn hand_computed_fixtures_bill_image_input_at_the_image_rate() {
    // 343 text in + 601 image out: ceil((343*5 + 601*30) / 1) micro-USD = 1_715 + 18_030.
    let u = normalize(&sunburst(), &stored_response());
    assert_eq!(u.micro_usd, Some(1_715 + 18_030));
    assert_eq!(u.micro_usd, Some(19_745));
    // 10 text + 40 image in, 50 image out: 10*5 + 40*8 + 50*30 = 50 + 320 + 1_500.
    let edit = normalize(&sunburst(), &official_minimal());
    assert_eq!(edit.micro_usd, Some(50 + 320 + 1_500));
    // Had the 40 image tokens been billed as text the total would be 50*5 + 1_500 = 1_750.
    assert_ne!(edit.micro_usd, Some(1_750));
    // Cached text (1.25) and cached image (2) differ from each other and from the plain
    // rates in the general calculator: 100 cached text only, 50 cached image only.
    let p = price(SUNBURST);
    let cached = Usage {
        model: SUNBURST.into(),
        input: Some(300),
        output: Some(0),
        cached: Some(150),
        image_input: Some(100),
        cached_image_input: Some(50),
        reasoning: Some(0),
        image_output: Some(0),
        ..Usage::default()
    };
    // ordinary text 300-150-100+50 = 100 *5; cached text 150-50 = 100 *1.25;
    // image only 100-50 = 50 *8; cached image 50 *2  => 500 + 125 + 400 + 100.
    assert_eq!(p.cost(&cached), Ok(500 + 125 + 400 + 100));
    // Direct Images API responses never carry these cache hits, so this stays a calculator
    // fixture: the Images normalization above reports cached counters of exactly 0.
}
