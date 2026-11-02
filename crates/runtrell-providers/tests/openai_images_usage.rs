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

const SUNBURST: &str = "gpt-image-2.5-sunburst";

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
    for yes in [
        "gpt-image-2",
        "gpt-image-2.5-sunburst",
        "gpt-image-2.5-flare",
        "gpt-image-2-2026-01-01",
        "gpt-image-2.6",
    ] {
        assert!(zero(yes), "{yes}");
    }
    for no in [
        "gpt-image-1",
        "gpt-image-20",
        "gpt-image-2x",
        "gpt-image-2.",
        "gpt-image-2.x",
        "image-gpt-image-2",
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
    body["model"] = json!("gpt-image-2.5-sunburst-2026-10-01");
    let unknown = normalize(&sunburst(), &body);
    assert_eq!(unknown.micro_usd, None);
    assert_eq!(unknown.model, "gpt-image-2.5-sunburst-2026-10-01");
    assert_eq!(
        unknown.unknown_reason.as_deref(),
        Some(images_usage::REASON_MODEL_UNPRICED)
    );
    let mut aliased = sunburst();
    aliased.capabilities.images.response_model_aliases =
        vec!["gpt-image-2.5-sunburst-2026-10-01".into()];
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
