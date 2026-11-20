// SPDX-License-Identifier: Apache-2.0
use super::allowance::{AllowanceCharge, ProviderAllowanceEvidence};
use super::pricing::*;
use super::{
    CachePolicy, CachePolicyKind, Effect, ImageReservation, Index, ResolvedModel, validate,
};
use crate::{Amount, Error, ModelLimits, ReservationPolicy, Result, Usage};
use serde::{Deserialize, Serialize};
use serde_json::json;

pub const CACHE_WRITE: &str = "cache_write_tokens";
pub const CACHE_WRITE_30M: &str = "cache_write_30m_tokens";
pub const CACHE_WRITE_12H: &str = "cache_write_12h_tokens";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingSnapshot {
    catalog_revision: String,
    catalog_digest: String,
    model_id: String,
    binding_id: String,
    wire_model: String,
    route: String,
    usage_rule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    upstream_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    alternative_routes: Vec<PricingSnapshot>,
    response_aliases: Vec<String>,
    fixed: Cell,
    constrains: Condition,
    price_id: String,
    price: PriceProfile,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    additional_roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    additional_prices: Map<PriceProfile>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    allowances: Map<Allowance>,
    request_retention: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_reservation: Option<ImageReservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_policy: Option<CachePolicy>,
}

impl PricingSnapshot {
    pub(super) fn new(resolved: &ResolvedModel, index: &Index) -> Self {
        let mut effects = resolved.binding.effects.values();
        let mut snapshot =
            Self::for_effect(resolved, index, effects.next().expect("verified effect"));
        snapshot.alternative_routes = effects
            .map(|effect| Self::for_effect(resolved, index, effect))
            .collect();
        snapshot
    }

    fn for_effect(resolved: &ResolvedModel, index: &Index, effect: &Effect) -> Self {
        let binding = resolved.binding;
        let price_id = &effect.prices[0];
        let additional_roots = effect.prices[1..].to_vec();
        let mut additional_prices = Map::new();
        let mut remaining = additional_roots.clone();
        while let Some(id) = remaining.pop() {
            if !additional_prices.contains_key(&id) && &id != price_id {
                let price = index.catalog.price_profiles[&id].clone();
                remaining.extend(price.fees.values().map(|fee| fee.base_profile.clone()));
                additional_prices.insert(id, price);
            }
        }
        let allowances = additional_prices
            .values()
            .flat_map(|profile| profile.fees.values())
            .filter_map(|fee| fee.allowance.as_ref())
            .map(|id| (id.clone(), index.catalog.allowances[id].clone()))
            .collect();
        Self {
            catalog_revision: index.catalog.catalog_revision.clone(),
            catalog_digest: index.digest.clone(),
            model_id: resolved.id().into(),
            binding_id: resolved.binding_id.clone(),
            wire_model: resolved.wire_model().into(),
            route: resolved.endpoint(),
            usage_rule: binding.usage_rule.clone(),
            upstream_provider: effect.upstream_provider.clone(),
            alternative_routes: Vec::new(),
            response_aliases: binding.response_aliases.clone(),
            fixed: validate::fixed_cell(resolved.profile, binding, effect),
            constrains: binding.constrains.clone(),
            price_id: price_id.clone(),
            price: index.catalog.price_profiles[price_id].clone(),
            additional_roots,
            additional_prices,
            allowances,
            request_retention: resolved.cache_retention_seconds(),
            image_reservation: binding.image_reservation.clone(),
            cache_policy: binding.cache_policy.clone(),
        }
    }

    fn bill(
        &self,
        cell: Cell,
        measured: &Map<Vec<Item>>,
        reserving: bool,
        roots: &[String],
    ) -> Result<u64> {
        self.with_quote(cell, measured, |mut quote| {
            quote.reserving = reserving;
            quote.usd(roots)
        })
    }

    fn with_quote<T>(
        &self,
        cell: Cell,
        measured: &Map<Vec<Item>>,
        evaluate: impl FnOnce(Quote<'_>) -> Result<T>,
    ) -> Result<T> {
        let mut profiles = self.additional_prices.clone();
        profiles.insert(self.price_id.clone(), self.price.clone());
        evaluate(Quote {
            profiles: &profiles,
            cells: &profiles
                .keys()
                .map(|id| (id.clone(), cell.clone()))
                .collect(),
            time_windows: &Map::new(),
            measurements: &profiles
                .keys()
                .map(|id| (id.clone(), measured.clone()))
                .collect(),
            remaining: Map::new(),
            activated: true,
            reserving: false,
        })
    }

    fn reserve_roots(&self) -> Vec<String> {
        std::iter::once(self.price_id.clone())
            .chain(self.additional_roots.iter().cloned())
            .collect()
    }

    /// Maximum over price cells and over mutually exclusive input/output token
    /// classes. These verified adapters support linear token rates only.
    pub fn estimate(&self, input: u64, output: u64) -> Result<Amount> {
        let mut reserve = self.estimate_route(input, output)?;
        for route in &self.alternative_routes {
            let candidate = route.estimate_route(input, output)?;
            reserve.tokens = reserve.tokens.max(candidate.tokens);
            reserve.micro_usd = reserve.micro_usd.max(candidate.micro_usd);
        }
        Ok(reserve)
    }

    fn estimate_route(&self, input: u64, output: u64) -> Result<Amount> {
        let cells = validate::cells(&self.price, &self.fixed, &self.constrains, None)
            .map_err(|_| Error::ReservationDenied)?;
        let input_components = self.price.components.iter().filter(|(_, c)| {
            c.meter.direction == Direction::Input
                && c.meter
                    .retention_seconds
                    .is_none_or(|ttl| Some(ttl) == self.request_retention)
        });
        let output_components: Vec<_> = self
            .price
            .components
            .iter()
            .filter(|(_, c)| c.meter.direction == Direction::Output)
            .collect();
        let mut worst = None;
        let roots = self.reserve_roots();
        for cell in cells {
            if cell
                .get("prompt_tokens")
                .and_then(|v| v.as_u64())
                .is_some_and(|n| n > input)
                || cell
                    .get("context_tokens")
                    .and_then(|v| v.as_u64())
                    .is_some_and(|n| n > input.saturating_add(output))
            {
                continue;
            }
            for (input_id, _) in input_components.clone() {
                for (output_id, _) in &output_components {
                    let measurements = self
                        .price
                        .components
                        .keys()
                        .map(|id| {
                            let count = if id == input_id {
                                input
                            } else if id == *output_id {
                                output
                            } else {
                                0
                            };
                            (id.clone(), vec![Item::measured(count)])
                        })
                        .collect();
                    let cost = self.bill(cell.clone(), &measurements, true, &roots)?;
                    worst = Some(worst.map_or(cost, |previous: u64| previous.max(cost)));
                }
            }
        }
        Ok(Amount::new(
            input.checked_add(output).ok_or(Error::ReservationDenied)?,
            worst.ok_or(Error::ReservationDenied)?,
        ))
    }

    /// Budget for the exact fixed-size, single-image request. Commit the
    /// undiscounted price even when a cache read is possible.
    pub fn estimate_image(
        &self,
        size: &str,
        quality: &str,
        prompt_bytes: usize,
        references: u64,
    ) -> Result<Amount> {
        let rule = self
            .image_reservation
            .as_ref()
            .ok_or(Error::ReservationDenied)?;
        let edit = self.usage_rule == "openai.sunburst_direct_edit@v1";
        if prompt_bytes > rule.prompt_bytes || references != u64::from(edit) {
            return Err(Error::ReservationDenied);
        }
        let output = rule
            .calculator_output_tokens
            .get(size)
            .and_then(|row| row.get(quality))
            .and_then(|n| n.checked_mul(rule.output_multiplier))
            .ok_or(Error::ReservationDenied)?;
        let image = references
            .checked_mul(rule.reference_tokens)
            .ok_or(Error::ReservationDenied)?;
        self.image_amount(rule.text_tokens, image, output, 0, 0, true)
    }

    fn image_amount(
        &self,
        text: u64,
        image: u64,
        output: u64,
        text_cache: u64,
        image_cache: u64,
        reserving: bool,
    ) -> Result<Amount> {
        let quantities = [
            (
                "text-in",
                text.checked_sub(text_cache).ok_or(Error::UsageUnknown)?,
            ),
            ("text-cache", text_cache),
            (
                "image-in",
                image.checked_sub(image_cache).ok_or(Error::UsageUnknown)?,
            ),
            ("image-cache", image_cache),
            ("image-out", output),
        ];
        let mut measured = Map::new();
        for (id, count) in quantities {
            if self.price.components.contains_key(id) {
                measured.insert(id.to_owned(), vec![Item::measured(count)]);
            } else if count != 0 {
                return Err(Error::UsageUnknown);
            }
        }
        let tokens = text
            .checked_add(image)
            .and_then(|n| n.checked_add(output))
            .ok_or(Error::UsageUnknown)?;
        Ok(Amount::new(
            tokens,
            self.bill(
                self.fixed.clone(),
                &measured,
                reserving,
                std::slice::from_ref(&self.price_id),
            )?,
        ))
    }

    fn image_cost(&self, usage: &Usage, input: u64, output: u64) -> Result<Amount> {
        let text = usage
            .quantities
            .get("text_input_tokens")
            .copied()
            .ok_or(Error::UsageUnknown)?;
        let image = usage.image_input.ok_or(Error::UsageUnknown)?;
        let cached = usage
            .cached
            .or_else(|| self.inherited_cache_zero(usage).then_some(0))
            .ok_or(Error::UsageUnknown)?;
        let image_cache = usage
            .cached_image_input
            .or((cached == 0 || image == 0).then_some(0))
            .ok_or(Error::UsageUnknown)?;
        if text.checked_add(image) != Some(input)
            || usage.image_output != Some(output)
            || !matches!(usage.reasoning, None | Some(0))
            || usage
                .quantities
                .get("text_output_tokens")
                .is_some_and(|&n| n != 0)
        {
            return Err(Error::UsageUnknown);
        }
        let text_cache = cached.checked_sub(image_cache).ok_or(Error::UsageUnknown)?;
        self.image_amount(text, image, output, text_cache, image_cache, false)
    }

    pub(crate) fn cap(
        &self,
        limits: &ModelLimits,
        input: u64,
        remaining: Amount,
        policy: &ReservationPolicy,
    ) -> Result<(u64, Amount)> {
        let output_limit = limits.max_output;
        let max = output_limit.min(remaining.tokens.saturating_sub(input));
        let min = match policy {
            ReservationPolicy::Shrink {
                min_output_budget_tokens,
            } => limits.min_max_output_tokens.max(*min_output_budget_tokens),
            _ => output_limit,
        };
        if min == 0 || max < min || self.estimate(input, min)?.micro_usd > remaining.micro_usd {
            return Err(Error::ReservationDenied);
        }
        let (mut low, mut high) = (min, max);
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            if self.estimate(input, middle)?.micro_usd <= remaining.micro_usd {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        Ok((low, self.estimate(input, low)?))
    }

    /// Preserve the router-reported charge only with proof of the expected payer.
    pub fn record_openrouter_charge(
        &self,
        usage: &mut Usage,
        provider: &str,
        raw: &serde_json::Value,
    ) -> Result<()> {
        let (index, snapshot) = std::iter::once(self)
            .chain(&self.alternative_routes)
            .enumerate()
            .find(|(_, route)| {
                route
                    .upstream_provider
                    .as_deref()
                    .is_some_and(|name| name.eq_ignore_ascii_case(provider))
            })
            .ok_or(Error::UsageUnknown)?;
        let is_byok = raw["is_byok"].as_bool().ok_or(Error::UsageUnknown)?;
        let upstream = &raw["cost_details"]["upstream_inference_cost"];
        let byok = snapshot.fixed["mode"] == "byok";
        if is_byok != byok || (!byok && snapshot.fixed["mode"] != "routed") {
            return Err(Error::UsageUnknown);
        }
        let credits = reported_usd(&raw["cost"])?;
        if byok {
            usage.quantities.insert(
                "openrouter_upstream_micro_usd".into(),
                reported_usd(upstream)?,
            );
        } else if !upstream.is_null() && reported_usd(upstream)? != 0 {
            return Err(Error::UsageUnknown);
        }
        usage
            .quantities
            .insert("openrouter_credit_micro_usd".into(), credits);
        usage
            .quantities
            .insert("openrouter_route".into(), index as u64);
        Ok(())
    }

    /// Recomputes the charge from the original snapshot and measured counters.
    pub fn cost(&self, usage: &Usage) -> Result<Amount> {
        self.measured_route(usage)?.cost_for_route(usage)
    }

    fn measured_route(&self, usage: &Usage) -> Result<&Self> {
        if self.usage_rule == "openrouter.llama_chat@v1" {
            let index = usage
                .quantities
                .get("openrouter_route")
                .and_then(|&index| usize::try_from(index).ok())
                .ok_or(Error::UsageUnknown)?;
            std::iter::once(self)
                .chain(&self.alternative_routes)
                .nth(index)
                .ok_or(Error::UsageUnknown)
        } else {
            Ok(self)
        }
    }

    pub(crate) fn allowance_charge(
        &self,
        usage: &Usage,
        evidence: &ProviderAllowanceEvidence,
    ) -> Result<AllowanceCharge> {
        let route = self.measured_route(usage)?;
        if route.usage_rule != "openrouter.llama_chat@v1"
            || route.fixed["mode"] != "byok"
            || evidence.provider != "openrouter"
        {
            return Err(Error::Conflict);
        }
        // The verified BYOK binding has one fee over a list-price reference.
        // Other component/fee or rounding shapes need a separate mapping.
        let [root] = route.additional_roots.as_slice() else {
            return Err(Error::Conflict);
        };
        let profile = route.additional_prices.get(root).ok_or(Error::Conflict)?;
        let fee = profile.fees.get("platform").ok_or(Error::Conflict)?;
        let id = fee.allowance.as_deref().ok_or(Error::Conflict)?;
        let allowance = route.allowances.get(id).ok_or(Error::Conflict)?;
        let (cell, measured, _) = route.token_measurements(usage)?;
        if profile.billing_provider != evidence.provider
            || profile.fees.len() != 1
            || !profile.components.is_empty()
            || profile.charge_rule.is_some()
            || profile.time_window.is_some()
            || !matches(&fee.applies, &cell)
            || allowance.kind != "included_money"
            || allowance.scope != "account"
            || allowance.period != "month"
        {
            return Err(Error::Conflict);
        }
        let eligible = route.with_quote(cell, &measured, |mut quote| {
            quote.profile(&fee.base_profile, &mut Map::new(), &mut Default::default())
        })?;
        Ok(AllowanceCharge {
            debit: evidence.debit(id, &decimal(&allowance.amount)?, &eligible)?,
            factor: decimal(&fee.factor)?,
            reported_fee: *usage
                .quantities
                .get("openrouter_credit_micro_usd")
                .ok_or(Error::UsageUnknown)?,
        })
    }

    fn cost_for_route(&self, usage: &Usage) -> Result<Amount> {
        if (usage.model != self.wire_model && !self.response_aliases.contains(&usage.model))
            || usage.svg_usage.is_some()
        {
            return Err(Error::UsageUnknown);
        }
        let input = usage.input.ok_or(Error::UsageUnknown)?;
        let output = usage.output.ok_or(Error::UsageUnknown)?;
        usage.validate_subsets(input, output)?;
        if self.image_reservation.is_some() {
            return self.image_cost(usage, input, output);
        }
        let (cell, measured, tokens) = self.token_measurements(usage)?;
        let mut charge = self.bill(
            cell.clone(),
            &measured,
            false,
            std::slice::from_ref(&self.price_id),
        )?;
        if self.usage_rule == "openrouter.llama_chat@v1" {
            let credits = *usage
                .quantities
                .get("openrouter_credit_micro_usd")
                .ok_or(Error::UsageUnknown)?;
            if self.fixed["mode"] == "byok" {
                let maximum = self.bill(cell, &measured, true, &self.additional_roots)?;
                if usage.quantities.get("openrouter_upstream_micro_usd") != Some(&charge)
                    || credits > maximum
                {
                    return Err(Error::UsageUnknown);
                }
                charge = charge.checked_add(credits).ok_or(Error::UsageUnknown)?;
            } else if credits != charge {
                return Err(Error::UsageUnknown);
            }
        }
        Ok(Amount::new(tokens, charge))
    }

    fn token_measurements(&self, usage: &Usage) -> Result<(Cell, Map<Vec<Item>>, u64)> {
        let input = usage.input.ok_or(Error::UsageUnknown)?;
        let output = usage.output.ok_or(Error::UsageUnknown)?;
        let tokens = input.checked_add(output).ok_or(Error::UsageUnknown)?;
        let mut cell = self.fixed.clone();
        cell.insert("prompt_tokens".into(), json!(input));
        cell.insert("context_tokens".into(), json!(tokens));
        if let Some(ttl) = self.request_retention {
            cell.insert("retention_seconds".into(), json!(ttl));
        }
        if !matches(&self.constrains, &cell) {
            return Err(Error::UsageUnknown);
        }
        let cached = match self.usage_rule.as_str() {
            "gemini.generate_content@v1" | "openai.responses@v1" => {
                usage.cached.ok_or(Error::UsageUnknown)?
            }
            "openrouter.llama_chat@v1" if matches!(usage.cached, None | Some(0)) => 0,
            _ => return Err(Error::UsageUnknown),
        };
        let mut written = 0;
        if self.usage_rule == "openai.responses@v1" {
            let ttl = self.request_retention.ok_or(Error::UsageUnknown)?;
            let reported = usage.quantities.get(CACHE_WRITE).copied();
            let chosen = usage
                .quantities
                .get(if ttl == 1800 {
                    CACHE_WRITE_30M
                } else {
                    CACHE_WRITE_12H
                })
                .copied();
            written = reported.or(chosen).ok_or(Error::UsageUnknown)?;
            for (key, expected) in [(CACHE_WRITE_30M, 1800), (CACHE_WRITE_12H, 43200)] {
                if usage
                    .quantities
                    .get(key)
                    .is_some_and(|&count| count != if ttl == expected { written } else { 0 })
                {
                    return Err(Error::UsageUnknown);
                }
            }
        }
        let normal = input
            .checked_sub(cached)
            .and_then(|n| n.checked_sub(written))
            .ok_or(Error::UsageUnknown)?;
        let mut measured = Map::new();
        for (id, component) in &self.price.components {
            let meter = &component.meter;
            let count = match (meter.direction, meter.class.as_deref()) {
                (Direction::Input, Some("base")) => normal,
                (Direction::Input, Some("cache_read")) => cached,
                (Direction::Input, Some("cache_write")) => {
                    if meter.retention_seconds == self.request_retention {
                        written
                    } else {
                        0
                    }
                }
                (Direction::Output, Some("base")) => output,
                _ => return Err(Error::UsageUnknown),
            };
            measured.insert(id.clone(), vec![Item::measured(count)]);
        }
        Ok((cell, measured, tokens))
    }

    fn inherited_cache_zero(&self, usage: &Usage) -> bool {
        self.cache_policy.as_ref().is_some_and(|policy| {
            policy.kind == CachePolicyKind::InheritedDirectImage2And25ZeroWhenUnreported
        }) && usage.cached.is_none()
            && usage.cached_image_input.is_none()
    }

    pub fn record_cost(&self, usage: &mut Usage) {
        if self.inherited_cache_zero(usage) {
            usage.cached = Some(0);
            usage.cached_image_input = Some(0);
        }
        usage.micro_usd = self.cost(usage).ok().map(|cost| cost.micro_usd);
        if usage.micro_usd.is_none() && usage.unknown_reason.is_none() {
            usage.unknown_reason = Some("MANAGED_USAGE_UNKNOWN".into());
        }
    }
}
