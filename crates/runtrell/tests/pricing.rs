// SPDX-License-Identifier: Apache-2.0
use runtrell::*;

fn price() -> Price {
    Price {
        model: "sample".into(),
        input: 1_000_000,
        cached: 200_000,
        image_input: 2_000_000,
        cached_image_input: 200_000,
        output: 3_000_000,
        reasoning: 3_000_000,
        image_output: 4_000_000,
    }
}
fn usage() -> Usage {
    Usage {
        input: Some(100),
        output: Some(20),
        cached: Some(40),
        image_input: Some(30),
        cached_image_input: Some(10),
        image_output: Some(15),
        reasoning: Some(0),
        ..Usage::default()
    }
}
#[test]
fn intersection_cost_163_and_total_do_not_double_count_subsets() {
    assert_eq!(price().cost(&usage()), Ok(163));
    assert_eq!(usage().total(), Ok(120)); // cost observation is intentionally absent
    assert_eq!(usage().amount(), Err(Error::UsageUnknown));
    let mut u = usage();
    u.cached_image_input = None;
    assert_eq!(price().cost(&u), Err(Error::UsageUnknown));
    for cross in [31, 41] {
        u.cached_image_input = Some(cross);
        assert!(matches!(u.total(), Err(Error::Invalid(_))));
    }
    u.input = Some(50);
    u.cached_image_input = Some(19); // minimum valid intersection is 20
    assert!(matches!(u.total(), Err(Error::Invalid(_))));
    u.cached_image_input = Some(20);
    assert_eq!(u.total(), Ok(70));
}
#[test]
fn missing_totals_subsets_zero_and_overflow_are_distinct() {
    assert_eq!(Usage::default().total(), Err(Error::UsageUnknown));
    assert_eq!(Usage::measured(0, 0, 0).total(), Ok(0));
    assert_eq!(
        Usage::measured(u64::MAX, 1, 0).total(),
        Err(Error::UsageUnknown)
    );
    let mut u = usage();
    for field in 0..4 {
        let mut invalid = u.clone();
        match field {
            0 => invalid.cached = Some(101),
            1 => invalid.image_input = Some(101),
            2 => invalid.reasoning = Some(21),
            _ => invalid.image_output = Some(21),
        }
        assert!(matches!(invalid.total(), Err(Error::Invalid(_))));
    }
    u.image_output = None;
    assert_eq!(price().cost(&u), Err(Error::UsageUnknown));
    let mut p = price();
    p.input = u64::MAX;
    p.cached = u64::MAX;
    p.image_input = u64::MAX;
    p.cached_image_input = u64::MAX;
    p.output = u64::MAX;
    p.reasoning = u64::MAX;
    p.image_output = u64::MAX;
    assert_eq!(
        p.cost(&Usage::measured(u64::MAX, u64::MAX, 0)),
        Err(Error::ReservationDenied)
    );
    assert_eq!(
        p.estimate(u64::MAX, u64::MAX),
        Err(Error::ReservationDenied)
    );
}
#[test]
fn rounding_is_once_per_bill_and_quotes_use_worst_category() {
    let p = Price {
        input: 1,
        cached: 1,
        cached_image_input: 1,
        image_input: 1,
        output: 1,
        reasoning: 1,
        image_output: 1,
        ..price()
    };
    assert_eq!(p.cost(&usage()), Ok(1));
    assert_eq!(p.estimate(100, 20), Ok(Amount::new(120, 1)));
    assert_eq!(price().estimate(1000, 200), Ok(Amount::new(1200, 2800)));
    let limits = limits();
    assert_eq!(limits.estimate(100, 20), Ok(Amount::new(120, 280)));
    let (cap, quote) = limits.cap(100, Amount::new(120, 280)).unwrap();
    assert_eq!((cap, quote), (20, limits.estimate(100, 20).unwrap()));
}
fn limits() -> ModelLimits {
    ModelLimits {
        model: "sample".into(),
        context: 1000,
        max_output: 200,
        min_max_output_tokens: 3,
        price: Some(price()),
        timeout: 1,
    }
}
#[test]
fn technical_and_quality_floors_constrain_allowance_not_actual_generation() {
    let limits = limits();
    let policy = ReservationPolicy::Shrink {
        min_output_budget_tokens: 7,
    };
    assert_eq!(
        limits.cap_for_policy(100, Amount::new(106, 1000), &policy),
        Err(Error::ReservationDenied)
    );
    assert_eq!(
        limits
            .cap_for_policy(100, Amount::new(107, 1000), &policy)
            .unwrap()
            .0,
        7
    );
    let small = ReservationPolicy::Shrink {
        min_output_budget_tokens: 1,
    };
    assert_eq!(
        limits.cap_for_policy(100, Amount::new(102, 1000), &small),
        Err(Error::ReservationDenied)
    );
    assert_eq!(
        limits
            .cap_for_policy(100, Amount::new(103, 1000), &small)
            .unwrap()
            .0,
        3
    );
    // An early stop with zero generated tokens remains valid usage.
    let mut actual = Usage::measured(100, 0, 100);
    actual.cached = Some(0);
    actual.image_input = Some(0);
    assert_eq!(price().cost(&actual), Ok(100));
    assert_eq!(actual.total(), Ok(100));
    assert_eq!(
        limits.cap_for_policy(100, Amount::new(107, 1000), &ReservationPolicy::EndRun),
        Err(Error::ReservationDenied)
    );
}
#[test]
fn invalid_request_bounds_and_model_identity_fail_before_reservation() {
    let mut limits = limits();
    for (input, output) in [(1000, 3), (100, 201), (100, 2)] {
        assert_eq!(
            limits.estimate(input, output),
            Err(Error::ReservationDenied)
        );
    }
    limits.price = None;
    assert_eq!(limits.estimate(100, 20), Err(Error::PriceNotConfigured));
    limits.price = Some(Price {
        model: "other".into(),
        ..price()
    });
    assert_eq!(
        limits.cap(100, Amount::new(1000, 1000)),
        Err(Error::PriceNotConfigured)
    );
    limits.price = Some(price());
    limits.min_max_output_tokens = 201;
    assert!(matches!(
        limits.cap(100, Amount::new(1000, 1000)),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn every_valid_small_partition_is_bounded_by_the_quote() {
    for p in [
        price(),
        Price {
            cached_image_input: 9_000_000,
            reasoning: 8_000_000,
            ..price()
        },
    ] {
        for input in 0..=4 {
            for cached in 0..=input {
                for image in 0..=input {
                    for cross in 0..=cached.min(image) {
                        if cached + image > input + cross {
                            continue;
                        }
                        for output in 0..=4 {
                            for reasoning in 0..=output {
                                for image_output in 0..=output {
                                    let u = Usage {
                                        input: Some(input),
                                        output: Some(output),
                                        cached: Some(cached),
                                        image_input: Some(image),
                                        cached_image_input: Some(cross),
                                        reasoning: Some(reasoning),
                                        image_output: Some(image_output),
                                        ..Usage::default()
                                    };
                                    assert_eq!(u.total(), Ok(input + output));
                                    // Overlap is unreported: with both subsets nonzero
                                    // and billed at their own rates the price is unknown.
                                    let own_rates =
                                        p.reasoning != p.output && p.image_output != p.output;
                                    if own_rates && reasoning > 0 && image_output > 0 {
                                        assert_eq!(p.cost(&u), Err(Error::UsageUnknown));
                                        continue;
                                    }
                                    assert!(
                                        p.cost(&u).unwrap()
                                            <= p.estimate(input, output).unwrap().micro_usd
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The billing unit is one billed attempt: every disjoint category is summed in
/// micro-USD x tokens first and the attempt is rounded up once. Rounding each
/// category separately would bill 2 here instead of 1.
#[test]
fn categories_are_summed_before_one_round_up_per_billed_attempt() {
    let price = Price {
        model: "half".into(),
        input: 500_000,
        cached: 500_000,
        image_input: 500_000,
        cached_image_input: 500_000,
        output: 500_000,
        reasoning: 500_000,
        image_output: 500_000,
    };
    // 1 input token and 1 output token at 0.5 micro-USD each = 1.0 micro-USD.
    assert_eq!(price.cost(&Usage::measured(1, 1, 0)), Ok(1));
    // 1 token at 0.5 micro-USD rounds up to 1 for the attempt.
    assert_eq!(price.cost(&Usage::measured(1, 0, 0)), Ok(1));
}

fn output_price(reasoning: u64, image_output: u64) -> Price {
    Price {
        model: "overlap".into(),
        input: 1_000_000,
        cached: 1_000_000,
        image_input: 1_000_000,
        cached_image_input: 1_000_000,
        output: 2_000_000,
        reasoning,
        image_output,
    }
}
fn overlapping_output() -> Usage {
    // Valid under the wire contract: each subset is <= output, the sum is not.
    Usage {
        input: Some(0),
        output: Some(10),
        cached: Some(0),
        image_input: Some(0),
        cached_image_input: Some(0),
        reasoning: Some(8),
        image_output: Some(6),
        ..Usage::default()
    }
}
#[test]
fn reasoning_and_image_output_may_overlap_in_totals() {
    assert_eq!(overlapping_output().total(), Ok(10));
}
#[test]
fn reasoning_and_image_output_at_the_output_rate_do_not_need_the_overlap() {
    // Reasoning billed as output: only image output has its own rate.
    assert_eq!(
        output_price(2_000_000, 4_000_000).cost(&overlapping_output()),
        Ok(4 * 2 + 6 * 4)
    );
}
#[test]
fn reasoning_and_image_output_with_own_rates_and_unknown_overlap_are_refused() {
    assert_eq!(
        output_price(3_000_000, 4_000_000).cost(&overlapping_output()),
        Err(Error::UsageUnknown)
    );
}
