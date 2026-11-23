// SPDX-License-Identifier: Apache-2.0
//! Usage normalization for the OpenAI Images API (`/images/generations` and
//! `/images/edits`). It is separate from the Responses path: the Images response has
//! its own usage object. Reported cache details are preserved; missing cache
//! counters are inferred zero only for the exact 2/2.5 model list below.
//!
//! The public image usage schema describes total, input, output, text and image
//! counters. The guide confirms that 2 and 2.5 do not receive cache discounts
//! on direct Images requests.
//! <https://developers.openai.com/api/reference/resources/images/methods/generate>
//! <https://developers.openai.com/api/docs/guides/image-generation#cached-input-pricing>
//!
//! Managed 2.6 calls apply the catalog's inherited-structure policy when pricing
//! these counters. An explicit zero total or zero image input proves that the
//! cached-image subset is zero.
use crate::Config;
use runtrell::{Error, SvgUsage, Usage, UsageIssue};
#[cfg(test)]
use runtrell::{Price, SettlementRecovery};
use serde_json::Value;

/// Identifier of this rule set; recorded as the basis of an owner settlement recovery.
#[cfg(test)]
pub const BASIS: &str = "openai-images-usage-1";

pub const REASON_ABSENT: &str = "IMAGE_USAGE_ABSENT";
pub const REASON_MALFORMED: &str = "IMAGE_USAGE_MALFORMED";
pub const REASON_INCOMPLETE: &str = "IMAGE_USAGE_INCOMPLETE";
pub const REASON_INCONSISTENT: &str = "IMAGE_USAGE_INCONSISTENT";
pub const REASON_CACHED_UNREPORTED: &str = "IMAGE_USAGE_CACHED_UNREPORTED";
pub const REASON_MODEL_UNPRICED: &str = "IMAGE_USAGE_MODEL_UNPRICED";
pub const REASON_NO_PRICE: &str = "IMAGE_USAGE_NO_PRICE";
pub const REASON_OVERFLOW: &str = "IMAGE_USAGE_COST_OVERFLOW";
pub const REASON_PRICE_COUNTER_UNKNOWN: &str = "IMAGE_USAGE_PRICE_COUNTER_UNKNOWN";

/// Exact direct Images IDs used by the usage normalization fixtures.
/// The 2/2.5 dated entries are documented snapshots; no dated 2.6 ID is inferred.
pub const GPT_IMAGE_2_FAMILY: [&str; 6] = [
    "gpt-image-2",
    "gpt-image-2-2026-04-21",
    "gpt-image-2.5-sunburst",
    "gpt-image-2.5-sunburst-2026-09-08",
    "gpt-image-2.5-flare",
    "gpt-image-2.5-flare-2026-09-08",
];

/// True only for the exact IDs in [`GPT_IMAGE_2_FAMILY`].
pub fn cached_input_never_applies(model: &str) -> bool {
    GPT_IMAGE_2_FAMILY.contains(&model)
}

fn counter(usage: &Value, path: &str) -> Result<Option<u64>, ()> {
    match usage.pointer(path) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or(()),
    }
}

/// Normalizes the `usage` of one Images API response. Never fails: a response whose image
/// arrived but whose usage is not settleable returns counters it can prove and a reason.
#[cfg(test)]
pub(crate) fn normalize(config: &Config, response: &Value) -> Usage {
    normalize_with_svg(config, response, false)
}

/// SVG generation expects author evidence even when the provider omits it.
pub(crate) fn normalize_with_svg(config: &Config, response: &Value, expects_svg: bool) -> Usage {
    let mut usage = normalize_image(config, response);
    if expects_svg || response.get("svg_usage").is_some() {
        usage.svg_usage = Some(Box::new(normalize_svg(config, &response["svg_usage"])));
    }
    usage
}

fn normalize_svg(config: &Config, raw: &Value) -> SvgUsage {
    let mut svg = SvgUsage {
        reported: raw.clone(),
        model: raw["model"].as_str().map(str::to_owned),
        input: raw["input_tokens"].as_u64(),
        output: raw["output_tokens"].as_u64(),
        reasoning: raw["reasoning_tokens"].as_u64(),
        total: raw["total_tokens"].as_u64(),
        ..Default::default()
    };
    let reason = if raw.is_null() {
        Some("SVG_USAGE_ABSENT")
    } else if !raw.is_object()
        || [
            "input_tokens",
            "output_tokens",
            "reasoning_tokens",
            "total_tokens",
        ]
        .iter()
        .any(|k| {
            raw.get(k)
                .is_some_and(|v| !v.is_null() && v.as_u64().is_none())
        })
        || raw.get("model").is_some_and(|v| !v.is_string())
    {
        Some("SVG_USAGE_MALFORMED")
    } else if svg.model.as_deref().is_none_or(str::is_empty)
        || svg.input.is_none()
        || svg.output.is_none()
    {
        Some("SVG_USAGE_INCOMPLETE")
    } else if svg.token_total().is_err() {
        Some("SVG_USAGE_INCONSISTENT")
    } else {
        let mut prices = config
            .capabilities
            .images
            .svg_author_prices
            .iter()
            .filter(|p| Some(p.model.as_str()) == svg.model.as_deref());
        match (prices.next(), prices.next()) {
            (Some(price), None) => match price.cost(&svg) {
                Ok(cost) => {
                    svg.micro_usd = Some(cost);
                    svg.price = Some(price.clone());
                    None
                }
                Err(_) => Some("SVG_USAGE_PRICE_COUNTER_UNKNOWN"),
            },
            (None, _) => Some("SVG_USAGE_MODEL_UNPRICED"),
            _ => Some("SVG_USAGE_PRICE_AMBIGUOUS"),
        }
    };
    svg.unknown_reason = reason.map(Into::into);
    svg
}

fn normalize_image(config: &Config, response: &Value) -> Usage {
    let limits = &config.capabilities.limits;
    let aliases = &config.capabilities.images.response_model_aliases;
    // The response carries no model field in the published schema; absent means the
    // requested model. A different reported model is priced only if the host declared it
    // an alias of the requested model.
    let reported = response.get("model");
    let billed_as_requested = reported.is_none_or(|model| {
        model
            .as_str()
            .is_some_and(|m| m == limits.model || aliases.iter().any(|alias| alias == m))
    });
    let mut u = Usage {
        model: if billed_as_requested {
            limits.model.clone()
        } else {
            reported
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        },
        ..Usage::default()
    };
    let Some(usage) = response.get("usage").filter(|v| v.is_object()) else {
        u.unknown_reason = Some(REASON_ABSENT.into());
        u.issue = Some(UsageIssue::Missing);
        return u;
    };
    let parsed = (|| -> Result<_, ()> {
        Ok((
            counter(usage, "/total_tokens")?,
            counter(usage, "/input_tokens")?,
            counter(usage, "/output_tokens")?,
            counter(usage, "/input_tokens_details/text_tokens")?,
            counter(usage, "/input_tokens_details/image_tokens")?,
            counter(usage, "/output_tokens_details/image_tokens")?,
            counter(usage, "/output_tokens_details/text_tokens")?,
            counter(usage, "/input_tokens_details/cached_tokens")?,
            counter(
                usage,
                "/input_tokens_details/cached_tokens_details/image_tokens",
            )?,
        ))
    })();
    let Ok((total, input, output, in_text, in_image, out_image, out_text, cached, cached_image)) =
        parsed
    else {
        u.unknown_reason = Some(REASON_MALFORMED.into());
        u.issue = Some(UsageIssue::Invalid);
        return u;
    };
    // Report every counter the response gives, as given.
    u.input = input;
    u.output = output;
    u.image_input = in_image;
    u.cached = cached;
    u.cached_image_input = cached_image;
    if let Some(n) = in_text {
        u.quantities.insert("text_input_tokens".into(), n);
    }
    if let Some(n) = out_text {
        u.quantities.insert("text_output_tokens".into(), n);
    }
    // Spec: output_tokens are the image tokens of the output image; details, when present,
    // are authoritative and checked below.
    u.image_output = out_image.or_else(|| output.map(|o| o.saturating_sub(out_text.unwrap_or(0))));
    let consistent = match (input, in_text, in_image) {
        (Some(i), Some(t), Some(image)) => t.checked_add(image) == Some(i),
        _ => true,
    } && match (total, input, output) {
        (Some(t), Some(i), Some(o)) => i.checked_add(o) == Some(t),
        _ => true,
    } && in_text.zip(input).is_none_or(|(part, i)| part <= i)
        && in_image.zip(input).is_none_or(|(part, i)| part <= i)
        && input.zip(total).is_none_or(|(i, t)| i <= t)
        && output.zip(total).is_none_or(|(o, t)| o <= t)
        && u.validate_subsets(input.unwrap_or(u64::MAX), output.unwrap_or(u64::MAX))
            .is_ok()
        && match (output, out_image, out_text) {
            (Some(o), Some(i), Some(t)) => i.checked_add(t) == Some(o),
            (Some(o), Some(i), None) => i <= o,
            (Some(o), None, Some(t)) => t <= o,
            _ => true,
        };
    if !consistent {
        // Nothing here can be trusted: report no counter at all.
        u.input = None;
        u.output = None;
        u.image_input = None;
        u.image_output = None;
        u.quantities.clear();
        u.unknown_reason = Some(REASON_INCONSISTENT.into());
        u.issue = Some(UsageIssue::Invalid);
        return u;
    }
    let (Some(_), Some(_), Some(_), Some(_)) = (input, output, in_text, in_image) else {
        attach_price_evidence(config, &mut u, billed_as_requested);
        u.unknown_reason = Some(REASON_INCOMPLETE.into());
        u.issue = Some(UsageIssue::Incomplete);
        return u;
    };
    // Not filled: `reasoning`. The schema has no such category but also does not say none is
    // billed, so it stays unknown unless the host's price makes it irrelevant (reasoning rate
    // equal to the output rate).
    if cached_input_never_applies(&limits.model) && billed_as_requested {
        if u.cached.is_some_and(|n| n != 0) || u.cached_image_input.is_some_and(|n| n != 0) {
            u.unknown_reason = Some(REASON_INCONSISTENT.into());
            u.issue = Some(UsageIssue::Invalid);
            return u;
        }
        u.cached = u.cached.or(Some(0));
        u.cached_image_input = u.cached_image_input.or(Some(0));
    }
    if u.cached == Some(0) || (u.cached.is_some() && u.image_input == Some(0)) {
        u.cached_image_input = u.cached_image_input.or(Some(0));
    }
    price(config, &mut u, billed_as_requested);
    u
}

fn price(config: &Config, u: &mut Usage, billed_as_requested: bool) {
    let Some(price) = &config.capabilities.limits.price else {
        u.unknown_reason = Some(REASON_NO_PRICE.into());
        return;
    };
    if !billed_as_requested || price.model != u.model {
        u.unknown_reason = Some(REASON_MODEL_UNPRICED.into());
        u.issue = Some(UsageIssue::ModelBindingMismatch);
        return;
    }
    attach_price_evidence(config, u, billed_as_requested);
    match price.cost(u) {
        Ok(cost) => u.micro_usd = Some(cost),
        Err(Error::UsageUnknown) => {
            u.unknown_reason = Some(
                if u.cached.is_none() {
                    REASON_CACHED_UNREPORTED
                } else {
                    REASON_PRICE_COUNTER_UNKNOWN
                }
                .into(),
            );
        }
        Err(Error::ReservationDenied) => {
            u.unknown_reason = Some(REASON_OVERFLOW.into());
            u.issue = Some(UsageIssue::ArithmeticOverflow);
        }
        Err(_) => {
            u.unknown_reason = Some(REASON_INCONSISTENT.into());
            u.issue = Some(UsageIssue::Invalid);
        }
    }
}

fn attach_price_evidence(config: &Config, u: &mut Usage, billed_as_requested: bool) {
    if billed_as_requested
        && let Some(price) = config
            .capabilities
            .limits
            .price
            .as_ref()
            .filter(|p| p.model == u.model)
        && let Ok(evidence) = price.cost_breakdown(u)
    {
        u.price_evidence = Some(Box::new(evidence));
    }
}

/// Builds the owner settlement recovery for a stored Images wire fixture.
/// Returns `UsageUnknown` when the model family is not covered (the operation then stays for
/// the operator) and fills only the cache counters covered by that rule.
#[cfg(test)]
pub(crate) fn recovery_for_stored_usage(
    stored: &Usage,
    price: &Price,
) -> Result<SettlementRecovery, Error> {
    if !cached_input_never_applies(&stored.model) || stored.micro_usd.is_some() {
        return Err(Error::UsageUnknown);
    }
    let mut usage = stored.clone();
    usage.unknown_reason = None;
    usage.issue = None;
    for counter in [&mut usage.cached, &mut usage.cached_image_input] {
        match counter {
            None => *counter = Some(0),
            Some(0) => {}
            Some(_) => {
                return Err(Error::Invalid(
                    "cached tokens on the Images endpoint".into(),
                ));
            }
        }
    }
    Ok(SettlementRecovery {
        basis: BASIS.into(),
        usage,
        price: Some(price.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{Adapter, Wire};
    use crate::{
        ApiKey, Capabilities, CounterSupport, DocumentCapabilities, DocumentLimits,
        ImageCapabilities, ImageLimits,
    };
    use runtrell::{ModelLimits, RetryPolicy, RunHandle};
    use serde_json::json;

    const MODEL: &str = "gpt-image-2.5-sunburst";
    fn config() -> Config {
        Config {
            endpoint: "https://api.example.test/v1/images/generations".into(),
            key: ApiKey::new("TEST-KEY-DO-NOT-PRINT"),
            capabilities: Capabilities {
                limits: ModelLimits {
                    model: MODEL.into(),
                    context: 8192,
                    max_output: 4096,
                    min_max_output_tokens: 1,
                    price: Some(Price {
                        model: MODEL.into(),
                        input: 5_000_000,
                        cached: 1_250_000,
                        output: 30_000_000,
                        reasoning: 30_000_000,
                        image_input: 8_000_000,
                        image_output: 30_000_000,
                        cached_image_input: 2_000_000,
                    }),
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
            retry_policy: RetryPolicy {
                max_attempts: 3,
                backoff: 1_000,
                max_elapsed: 60_000,
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

    /// Reproduces the stored defect: the Responses usage path leaves the cost unknown for
    /// an Images response, because the endpoint reports no cached counter.
    #[test]
    fn the_responses_adapter_path_left_this_response_unpriced_and_the_images_path_does_not() {
        let body = json!({"data":[{"b64_json":"x"}],"usage":{"total_tokens":944,
            "input_tokens":343,"output_tokens":601,
            "input_tokens_details":{"text_tokens":343,"image_tokens":0},
            "output_tokens_details":{"image_tokens":601,"text_tokens":0}}});
        let old = Adapter::new(config(), Wire::Responses)
            .unwrap()
            .usage(&body)
            .unwrap();
        assert_eq!(old.cached, None);
        assert_eq!(old.micro_usd, None);
        assert_eq!(
            (old.input, old.output, old.image_output),
            (Some(343), Some(601), Some(601))
        );
        let new = normalize(&config(), &body);
        assert_eq!(new.cached, Some(0));
        assert_eq!(new.micro_usd, Some(19_745));
    }
}
