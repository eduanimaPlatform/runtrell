// SPDX-License-Identifier: Apache-2.0
use super::validate::TimeWindow;
use crate::{Error, Result};
use num_bigint::BigInt;
pub(super) use num_rational::BigRational as Money;
use num_traits::{ToPrimitive, Zero};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) type Map<T> = BTreeMap<String, T>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Token,
    Image,
    Pixel,
    Artifact,
    MediaMs,
    Character,
    Request,
    ToolCall,
    SearchQuery,
    Page,
    Record,
    SizeMs,
    ResourceTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Direction {
    Input,
    Output,
    Neutral,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Meter {
    pub unit: Unit,
    pub direction: Direction,
    pub modalities: Option<Vec<String>>,
    pub class: Option<String>,
    pub retention_seconds: Option<u64>,
    pub item_kind: Option<String>,
    pub character_rule: Option<String>,
    pub time_basis: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum Predicate {
    Range {
        min: u64,
        max_exclusive: Option<u64>,
    },
    OneOf(Vec<Value>),
}

impl Predicate {
    pub(crate) fn contains(&self, value: &Value) -> bool {
        match self {
            Self::Range { min, max_exclusive } => value
                .as_u64()
                .is_some_and(|n| n >= *min && max_exclusive.is_none_or(|end| n < end)),
            Self::OneOf(values) => values.contains(value),
        }
    }
}

pub(crate) type Condition = Map<Predicate>;
pub(crate) type Cell = Map<Value>;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Selector {
    #[serde(default)]
    pub when: Condition,
    pub effective: Option<Period>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Period {
    pub from: String,
    pub until: Option<String>,
}

impl Selector {
    pub fn matches(&self, cell: &Cell) -> bool {
        matches(&self.when, cell)
            && self.effective.as_ref().is_none_or(|period| {
                cell.get("effective")
                    .and_then(Value::as_str)
                    .is_some_and(|time| {
                        timestamp(&period.from) <= timestamp(time)
                            && period
                                .until
                                .as_ref()
                                .is_none_or(|end| timestamp(time) < timestamp(end))
                    })
            })
    }
}

pub(crate) fn timestamp(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|v| v.timestamp_millis())
}

pub(crate) fn matches(condition: &Condition, cell: &Cell) -> bool {
    condition.iter().all(|(axis, predicate)| {
        cell.get(axis)
            .is_some_and(|value| predicate.contains(value))
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PriceProfile {
    pub billing_provider: String,
    pub payer: String,
    pub currency: String,
    pub purpose: String,
    pub clock: String,
    pub time_window: Option<String>,
    pub charge_rule: Option<ChargeRule>,
    #[serde(default)]
    pub components: Map<Component>,
    #[serde(default)]
    pub factor_groups: Map<Factors>,
    #[serde(default)]
    pub fees: Map<Fee>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Component {
    pub meter: Meter,
    pub bill_on: BillOn,
    pub rounding_known: bool,
    pub rates: Vec<Rate>,
    #[serde(default)]
    pub applies: Condition,
    #[serde(default)]
    pub quantization: Vec<Quantization>,
    pub allowance: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillOn {
    Measured,
    Accepted,
    Completed,
    Success,
    Grounded,
    BillableActivation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Rate {
    pub state: RateState,
    pub per: Option<String>,
    pub amount: Option<String>,
    pub marginal: Option<Vec<Marginal>>,
    #[serde(flatten)]
    pub selector: Selector,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RateState {
    Priced,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Marginal {
    pub quantity: QuantityRange,
    pub amount: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct QuantityRange {
    pub min: String,
    pub max_exclusive: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Factors {
    pub components: Vec<String>,
    pub rows: Vec<Factor>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Factor {
    pub factor: String,
    #[serde(flatten)]
    pub selector: Selector,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Fee {
    pub base_profile: String,
    pub factor: String,
    pub allowance: Option<String>,
    #[serde(default)]
    pub applies: Condition,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Allowance {
    pub kind: String,
    pub scope: String,
    pub period: String,
    pub zone: Option<String>,
    pub amount: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Attempt,
    Item,
    Event,
    Session,
    Resource,
    BillingPeriod,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Axis {
    Quantity,
    Size,
    Duration,
    Count,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Round {
    Ceil,
    Floor,
    HalfUp,
    Exact,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Quantization {
    pub axis: Axis,
    pub scope: Scope,
    pub round: Round,
    pub step: String,
    pub minimum: String,
    pub trigger: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChargeRule {
    pub scope: Scope,
    pub quantum: String,
    pub minimum: String,
    pub round: Round,
    pub trigger: Option<String>,
}

/// A billable quantity already separated into its provider billing scope.
/// Size/time and count/time are rounded on each axis before multiplication.
#[derive(Clone, Debug)]
pub(crate) struct Item {
    pub amount: Measured,
    pub scope: Scope,
    /// Stable provider item or accounting period ID, shared across components.
    pub group: Option<String>,
    pub eligible: BTreeSet<BillOn>,
}

#[derive(Clone, Debug)]
pub enum Measured {
    Units(u128),
    SizeTime { size: u128, ms: u128 },
    CountTime { count: u128, ms: u128 },
    NotApplicable,
    Unknown,
}

impl Ord for BillOn {
    fn cmp(&self, rhs: &Self) -> std::cmp::Ordering {
        (*self as u8).cmp(&(*rhs as u8))
    }
}
impl PartialOrd for BillOn {
    fn partial_cmp(&self, rhs: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(rhs))
    }
}

impl Item {
    pub(crate) fn measured(quantity: u64) -> Self {
        Self {
            amount: Measured::Units(quantity.into()),
            scope: Scope::Attempt,
            group: None,
            eligible: BTreeSet::from([BillOn::Measured]),
        }
    }
}

pub(crate) fn decimal(value: &str) -> Result<Money> {
    let (integer, fraction) = value.split_once('.').unwrap_or((value, ""));
    let digits = format!("{integer}{fraction}");
    let numerator = digits
        .parse::<BigInt>()
        .map_err(|_| Error::PriceNotConfigured)?;
    if numerator < BigInt::ZERO || fraction.len() > 24 || integer.len() > 36 {
        return Err(Error::PriceNotConfigured);
    }
    Ok(Money::new(
        numerator,
        BigInt::from(10u8).pow(fraction.len() as u32),
    ))
}

/// Convert a provider's nonnegative USD number to the ledger unit. JSON
/// numbers may be serialized in scientific notation, even for very small fees.
pub(super) fn reported_usd(value: &Value) -> Result<u64> {
    let value = value.as_number().ok_or(Error::UsageUnknown)?.to_string();
    let (mantissa, exponent) = match value.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (
            mantissa,
            exponent.parse::<i32>().map_err(|_| Error::UsageUnknown)?,
        ),
        None => (value.as_str(), 0),
    };
    if !(-36..=36).contains(&exponent) {
        return Err(Error::UsageUnknown);
    }
    let mut amount = decimal(mantissa).map_err(|_| Error::UsageUnknown)?;
    let scale = BigInt::from(10u8).pow(exponent.unsigned_abs());
    if exponent < 0 {
        amount /= scale;
    } else {
        amount *= scale;
    }
    micro_usd(&amount).ok_or(Error::UsageUnknown)
}

const PICOS_PER_USD: u64 = 1_000_000_000_000;

pub(super) fn pico_usd(value: &Money) -> Result<u64> {
    let scaled = value * BigInt::from(PICOS_PER_USD);
    if !scaled.is_integer() {
        return Err(Error::UsageUnknown);
    }
    scaled.to_integer().to_u64().ok_or(Error::UsageUnknown)
}

pub(super) fn pico_usd_amount(value: u64) -> Money {
    Money::new(BigInt::from(value), BigInt::from(PICOS_PER_USD))
}

pub(super) fn micro_usd(value: &Money) -> Option<u64> {
    (value * BigInt::from(1_000_000u64))
        .ceil()
        .to_integer()
        .to_u64()
}

fn nonzero(value: &str) -> Result<Money> {
    let value = decimal(value)?;
    if value.is_zero() {
        return Err(Error::PriceNotConfigured);
    }
    Ok(value)
}

fn rounded(value: &Money, step: &Money, round: Round) -> Result<Money> {
    let units = value / step;
    let units = match round {
        Round::Ceil => units.ceil(),
        Round::Floor => units.floor(),
        Round::HalfUp => (units + Money::new(1.into(), 2.into())).floor(),
        Round::Exact if units.is_integer() => units,
        Round::Exact => return Err(Error::UsageUnknown),
    };
    Ok(units * step)
}

fn quantity(component: &Component, item: &Item) -> Result<Money> {
    if matches!(item.amount, Measured::NotApplicable) || !item.eligible.contains(&component.bill_on)
    {
        return Ok(Money::zero());
    }
    let axis = |axis, n| -> Result<Money> {
        let value = Money::from_integer(BigInt::from(n));
        let Some(rule) = component.quantization.iter().find(|rule| rule.axis == axis) else {
            return Ok(value);
        };
        if rule.scope != item.scope {
            return Err(Error::UsageUnknown);
        }
        if n == 0
            && !(rule.trigger.as_deref() == Some("billable_activation")
                && item.eligible.contains(&BillOn::BillableActivation))
        {
            return Ok(value);
        }
        Ok(rounded(&value, &nonzero(&rule.step)?, rule.round)?.max(decimal(&rule.minimum)?))
    };
    match item.amount {
        Measured::Units(n) => axis(Axis::Quantity, n),
        Measured::SizeTime { size, ms } => Ok(axis(Axis::Size, size)? * axis(Axis::Duration, ms)?),
        Measured::CountTime { count, ms } => {
            Ok(axis(Axis::Count, count)? * axis(Axis::Duration, ms)?)
        }
        Measured::NotApplicable => Ok(Money::zero()),
        Measured::Unknown => Err(Error::UsageUnknown),
    }
}

impl Rate {
    fn bill(&self, quantity: &Money) -> Result<Money> {
        if quantity.is_zero() {
            return Ok(Money::zero());
        }
        if self.state == RateState::Unknown {
            return Err(Error::UsageUnknown);
        }
        let per = nonzero(self.per.as_deref().ok_or(Error::PriceNotConfigured)?)?;
        if let Some(amount) = &self.amount {
            return Ok(decimal(amount)? * quantity / per);
        }
        let mut subtotal = Money::zero();
        for band in self.marginal.as_ref().ok_or(Error::PriceNotConfigured)? {
            let end = band
                .quantity
                .max_exclusive
                .as_deref()
                .map(decimal)
                .transpose()?;
            let upper = end.as_ref().map_or(quantity, |end| quantity.min(end));
            let used = upper - decimal(&band.quantity.min)?;
            if used > Money::zero() {
                subtotal += used * decimal(&band.amount)?;
            }
        }
        Ok(subtotal / per)
    }
}

pub(crate) fn unique<T>(entries: &[T], predicate: impl Fn(&T) -> bool) -> Result<&T> {
    let mut selected = entries.iter().filter(|value| predicate(value));
    let one = selected.next().ok_or(Error::UsageUnknown)?;
    if selected.next().is_some() {
        return Err(Error::UsageUnknown);
    }
    Ok(one)
}

pub(crate) struct Quote<'a> {
    pub profiles: &'a Map<PriceProfile>,
    pub cells: &'a Map<Cell>,
    pub time_windows: &'a Map<TimeWindow>,
    pub measurements: &'a Map<Map<Vec<Item>>>,
    /// Certified remaining balance for a single quote. An absent balance does
    /// not provide a discount. A settlement that may depend on it stays pending.
    pub remaining: Map<Money>,
    pub activated: bool,
    pub reserving: bool,
}

impl Quote<'_> {
    fn deduct(&mut self, id: Option<&str>, amount: Money) -> Result<Money> {
        let Some(id) = id.filter(|_| !amount.is_zero()) else {
            return Ok(amount);
        };
        match self.remaining.get_mut(id) {
            Some(balance) => {
                let discount = amount.clone().min(balance.clone());
                *balance -= &discount;
                Ok(amount - discount)
            }
            None if self.reserving => Ok(amount),
            None => Err(Error::UsageUnknown),
        }
    }

    fn component(
        &mut self,
        profile: &PriceProfile,
        id: &str,
        component: &Component,
        cell: &Cell,
        items: &[Item],
    ) -> Result<Money> {
        if !matches(&component.applies, cell) {
            return Ok(Money::zero());
        }
        let rate = unique(&component.rates, |row| row.selector.matches(cell))?;
        if rate
            .amount
            .as_deref()
            .map(decimal)
            .transpose()?
            .is_some_and(|v| v.is_zero())
        {
            return Ok(Money::zero());
        }
        if !component.rounding_known {
            return Err(Error::UsageUnknown);
        }
        let measured = items.iter().try_fold(Money::zero(), |total, item| {
            Ok(total + quantity(component, item)?)
        })?;
        let measured = self.deduct(component.allowance.as_deref(), measured)?;
        let mut subtotal = rate.bill(&measured)?;
        for group in profile
            .factor_groups
            .values()
            .filter(|g| g.components.iter().any(|c| c == id))
        {
            let factor = unique(&group.rows, |row| row.selector.matches(cell))?;
            subtotal *= decimal(&factor.factor)?;
        }
        Ok(subtotal)
    }

    pub(super) fn profile(
        &mut self,
        id: &str,
        evaluated: &mut Map<Money>,
        visiting: &mut BTreeSet<String>,
    ) -> Result<Money> {
        if let Some(value) = evaluated.get(id) {
            return Ok(value.clone());
        }
        if !visiting.insert(id.into()) {
            return Err(Error::PriceNotConfigured);
        }
        let profile = self.profiles.get(id).ok_or(Error::PriceNotConfigured)?;
        let mut cell = self.cells.get(id).ok_or(Error::UsageUnknown)?.clone();
        if let Some(window_id) = &profile.time_window {
            let window = self
                .time_windows
                .get(window_id)
                .ok_or(Error::PriceNotConfigured)?;
            if let Some(instant) = cell
                .get("effective")
                .and_then(Value::as_str)
                .and_then(timestamp)
                && let Some(band) = window.band_at(instant)
            {
                cell.insert("time_band".into(), Value::String(band.into()));
            }
            if !cell.contains_key("time_band") {
                return Err(Error::UsageUnknown);
            }
        }
        let counters = self.measurements.get(id);
        let scope = profile
            .charge_rule
            .as_ref()
            .map_or(Scope::Attempt, |rule| rule.scope);
        let mut subtotals = BTreeMap::<Option<String>, Money>::new();
        for (id, component) in &profile.components {
            if !matches(&component.applies, &cell) {
                continue;
            }
            let unknown = [Item {
                amount: Measured::Unknown,
                scope: Scope::Attempt,
                group: None,
                eligible: BTreeSet::from([component.bill_on]),
            }];
            let items = counters
                .and_then(|c| c.get(id))
                .map_or(unknown.as_slice(), Vec::as_slice);
            let mut groups = BTreeMap::<Option<String>, Vec<Item>>::new();
            for item in items {
                let group = if scope == Scope::Attempt {
                    None
                } else {
                    if item.scope != scope || item.group.is_none() {
                        return Err(Error::UsageUnknown);
                    }
                    item.group.clone()
                };
                groups.entry(group).or_default().push(item.clone());
            }
            for (group, items) in groups {
                *subtotals.entry(group).or_default() +=
                    self.component(profile, id, component, &cell, &items)?;
            }
        }
        for fee in profile
            .fees
            .values()
            .filter(|fee| matches(&fee.applies, &cell))
        {
            if scope != Scope::Attempt {
                return Err(Error::UsageUnknown);
            }
            let base = self.profile(&fee.base_profile, evaluated, visiting)?;
            *subtotals.entry(None).or_default() +=
                self.deduct(fee.allowance.as_deref(), base)? * decimal(&fee.factor)?;
        }
        if self.activated && scope == Scope::Attempt {
            subtotals.entry(None).or_default();
        }
        let mut subtotal = Money::zero();
        for group in subtotals.into_values() {
            subtotal += match &profile.charge_rule {
                Some(rule)
                    if !group.is_zero()
                        || (self.activated
                            && rule.trigger.as_deref() == Some("billable_activation")) =>
                {
                    rounded(&group, &nonzero(&rule.quantum)?, rule.round)?
                        .max(decimal(&rule.minimum)?)
                }
                _ => group,
            };
        }
        visiting.remove(id);
        evaluated.insert(id.into(), subtotal.clone());
        Ok(subtotal)
    }

    pub(crate) fn usd(mut self, roots: &[String]) -> Result<u64> {
        let mut evaluated = Map::new();
        let mut subtotals = BTreeMap::<(String, String), Money>::new();
        let mut billed = BTreeSet::new();
        for id in roots {
            let profile = self.profiles.get(id).ok_or(Error::PriceNotConfigured)?;
            if profile.currency != "USD" || profile.purpose != "billable" || !billed.insert(id) {
                return Err(Error::PriceNotConfigured);
            }
            *subtotals
                .entry((profile.billing_provider.clone(), profile.payer.clone()))
                .or_default() += self.profile(id, &mut evaluated, &mut BTreeSet::new())?;
        }
        subtotals.values().try_fold(0u64, |sum, value| {
            let cost = micro_usd(value).ok_or(Error::ReservationDenied)?;
            sum.checked_add(cost).ok_or(Error::ReservationDenied)
        })
    }
}
