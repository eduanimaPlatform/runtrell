// SPDX-License-Identifier: Apache-2.0

use crate::{
    AccountPolicy, OwnerCommand, OwnerReceipt, PhaseConfig, PhaseInput, PhaseSeal, PhaseState,
    PreparedModelRequest, ReservationPolicy, SchedulingPolicy, SettlementRecord,
    SettlementRecovery,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("conflicting digest or revision")]
    Conflict,
    #[error("run is not under an operator hold")]
    NotHeld,
    #[error("owner generation or lease lost")]
    OwnershipLost,
    #[error("run closed")]
    Closed,
    #[error("deadline reached")]
    Deadline,
    #[error("budget at limit")]
    BudgetAtLimit,
    #[error("cannot safely reserve next call")]
    ReservationDenied,
    #[error("no progress: {0:?}")]
    NoProgress(ProgressStop),
    #[error("usage unavailable")]
    UsageUnknown,
    #[error("price not configured")]
    PriceNotConfigured,
    #[error("permission denied")]
    PermissionDenied,
    #[error("unsupported capability: {0}")]
    Unsupported(String),
    #[error("finite queue or slots full")]
    Backpressure,
    #[error("queue wait timed out")]
    QueueTimeout,
    #[error("machine wait: {0}")]
    Pending(String),
    #[error("request exceeds its byte limit")]
    RequestByteLimit,
    #[error("not found")]
    NotFound,
    #[error("invalid input: {0}")]
    Invalid(String),
    /// Only input errors that the calling agent can correct. Host configuration
    /// and storage failures must not enter the repair loop.
    #[error("invalid tool input at {path}: {hint}")]
    InvalidToolInput { path: String, hint: String },
    #[error("storage failed")]
    Storage,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Amount {
    pub tokens: u64,
    pub micro_usd: u64,
}
impl Amount {
    pub const fn new(tokens: u64, micro_usd: u64) -> Self {
        Self { tokens, micro_usd }
    }
    pub fn checked_add(self, rhs: Self) -> Result<Self> {
        Ok(Self::new(
            self.tokens
                .checked_add(rhs.tokens)
                .ok_or(Error::ReservationDenied)?,
            self.micro_usd
                .checked_add(rhs.micro_usd)
                .ok_or(Error::ReservationDenied)?,
        ))
    }
    pub fn subtract(self, rhs: Self) -> Self {
        Self::new(self.tokens - rhs.tokens, self.micro_usd - rhs.micro_usd)
    }
    pub fn reaches(self, limit: Self) -> bool {
        self.tokens >= limit.tokens || self.micro_usd >= limit.micro_usd
    }
    pub fn exceeds(self, limit: Self) -> bool {
        self.tokens > limit.tokens || self.micro_usd > limit.micro_usd
    }
}
/// Safe fixed reason code, pointer-sized so `Usage` (copied through every async frame) stays small.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
#[allow(clippy::box_collection)] // deliberate: a thin pointer keeps `Usage` small
pub struct UnknownReason(Box<String>);
impl From<&str> for UnknownReason {
    fn from(value: &str) -> Self {
        Self(Box::new(value.to_owned()))
    }
}
impl std::ops::Deref for UnknownReason {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Total input tokens, including cached and image input.
    pub input: Option<u64>,
    /// Total output tokens, including reasoning and image output.
    pub output: Option<u64>,
    /// All cached input tokens, image tokens included. `None` is unknown, never 0.
    pub cached: Option<u64>,
    pub reasoning: Option<u64>,
    /// All image input tokens, cached ones included.
    pub image_input: Option<u64>,
    pub image_output: Option<u64>,
    /// Input tokens that are both cached and image tokens (the intersection).
    pub cached_image_input: Option<u64>,
    pub quantities: BTreeMap<String, u64>,
    pub micro_usd: Option<u64>,
    pub model: String,
    /// Separate SVG author bill. Image counters and `micro_usd` above exclude it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub svg_usage: Option<Box<SvgUsage>>,
    /// Informational primary model price components; never used by `amount` or settlement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_evidence: Option<Box<PriceEvidence>>,
    /// Safe, fixed reason code (uppercase ASCII, no payload text) saying why a counter
    /// or the cost is unknown for a response that otherwise arrived. Adapters set it
    /// only when they deliberately keep a value unknown; it never turns unknown into 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_reason: Option<UnknownReason>,
    /// Proof that this attempt incurred no provider or tool charge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub non_billable: Option<NonBillableReason>,
    /// A typed explanation when reported counters cannot be settled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<UsageIssue>,
    /// Billing evidence supplied by a trusted verifier for this provider receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_allowance: Option<Box<crate::models::ProviderAllowanceEvidence>>,
    /// Confirmed provider expiry of the resource named by the Applied external ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_expires_at_ms: Option<u64>,
}
/// Independently computable primary model bill components. Each is rounded up
/// separately and must not be summed to infer the complete bill, which rounds once.
/// `None` means the component could not be proved from the reported counters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceEvidence {
    pub price: Price,
    pub input_micro_usd: Option<u64>,
    pub output_micro_usd: Option<u64>,
}
impl Usage {
    pub fn non_billable(reason: NonBillableReason) -> Self {
        Self {
            non_billable: Some(reason),
            ..Self::measured(0, 0, 0)
        }
    }

    pub fn outcome(&self) -> UsageOutcome<'_> {
        if let Some(reason) = self.non_billable
            && self.amount() == Ok(Amount::default())
        {
            return UsageOutcome::NonBillable(reason);
        }
        if self.amount().is_ok() {
            return UsageOutcome::Measured(self);
        }
        UsageOutcome::Unresolved {
            reason: self
                .issue
                .unwrap_or_else(|| match (self.input, self.output) {
                    (None, None) => UsageIssue::Missing,
                    (Some(_), Some(_)) if self.micro_usd.is_none() => {
                        UsageIssue::PricingUnavailable
                    }
                    (Some(_), Some(_)) => UsageIssue::Invalid,
                    _ => UsageIssue::Incomplete,
                }),
            reported: Some(self),
        }
    }

    pub fn measured(input: u64, output: u64, cost: u64) -> Self {
        Self {
            input: Some(input),
            output: Some(output),
            micro_usd: Some(cost),
            ..Self::default()
        }
    }
    pub fn validate_subsets(&self, i: u64, o: u64) -> Result<()> {
        let c = self.cached.unwrap_or(0);
        let image = self.image_input.unwrap_or(0);
        let cross = self.cached_image_input.unwrap_or(0);
        if c > i
            || image > i
            || self.reasoning.unwrap_or(0) > o
            || self.image_output.unwrap_or(0) > o
            || self.cached_image_input.is_some_and(|_| {
                cross > self.cached.unwrap_or(i).min(self.image_input.unwrap_or(i))
                    || u128::from(c) + u128::from(image) > u128::from(i) + u128::from(cross)
            })
        {
            return Err(Error::Invalid("usage subset".into()));
        }
        Ok(())
    }
    pub fn amount(&self) -> Result<Amount> {
        if self.issue.is_some() {
            return Err(Error::UsageUnknown);
        }
        let image_cost = self.micro_usd.ok_or(Error::UsageUnknown)?;
        let svg_cost = match &self.svg_usage {
            None => 0,
            Some(svg) => svg.cost()?,
        };
        Ok(Amount::new(
            self.total()?,
            image_cost
                .checked_add(svg_cost)
                .ok_or(Error::UsageUnknown)?,
        ))
    }
    /// Input + output, counting subsets only once; independent of measured cost.
    /// Missing totals or overflow are unknown, never measured zero.
    pub fn total(&self) -> Result<u64> {
        let i = self.input.ok_or(Error::UsageUnknown)?;
        let o = self.output.ok_or(Error::UsageUnknown)?;
        self.validate_subsets(i, o)?;
        i.checked_add(o)
            .and_then(|t| {
                t.checked_add(
                    self.svg_usage
                        .as_ref()
                        .map_or(Ok(0), |svg| svg.token_total())
                        .ok()?,
                )
            })
            .ok_or(Error::UsageUnknown)
    }
}

/// Separately reported author counters, with the entire original JSON retained even
/// when malformed or unpriced, until explicit Run erasure removes opaque payload
/// fields while preserving accounting validity. `total` and `reasoning` are never
/// additional charges.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SvgUsage {
    pub model: Option<String>,
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub reasoning: Option<u64>,
    pub total: Option<u64>,
    pub reported: Value,
    pub micro_usd: Option<u64>,
    pub price: Option<SvgAuthorPrice>,
    pub unknown_reason: Option<UnknownReason>,
}
impl SvgUsage {
    pub fn cost(&self) -> Result<u64> {
        let cost = self.price.as_ref().ok_or(Error::UsageUnknown)?.cost(self)?;
        if self.micro_usd != Some(cost) {
            return Err(Error::UsageUnknown);
        }
        Ok(cost)
    }
    pub(crate) fn refines(&self, old: &Self) -> bool {
        self.reported == old.reported
            && self.model == old.model
            && self.input == old.input
            && self.output == old.output
            && self.reasoning == old.reasoning
            && self.total == old.total
            && old.micro_usd.is_none_or(|c| self.micro_usd == Some(c))
            && old
                .price
                .as_ref()
                .is_none_or(|p| self.price.as_ref() == Some(p))
    }
    pub fn token_total(&self) -> Result<u64> {
        // Recovery may add a verified rate, but cannot reinterpret malformed or
        // missing original counters as measured values.
        if !self.reported.is_object()
            || self.model.as_deref().is_none_or(str::is_empty)
            || self.reported["model"].as_str() != self.model.as_deref()
            || [
                ("input_tokens", self.input),
                ("output_tokens", self.output),
                ("reasoning_tokens", self.reasoning),
                ("total_tokens", self.total),
            ]
            .into_iter()
            .any(|(key, normalized)| match self.reported.get(key) {
                None | Some(Value::Null) => normalized.is_some(),
                Some(value) => value.as_u64().is_none() || value.as_u64() != normalized,
            })
        {
            return Err(Error::UsageUnknown);
        }
        let total = self
            .input
            .ok_or(Error::UsageUnknown)?
            .checked_add(self.output.ok_or(Error::UsageUnknown)?)
            .ok_or(Error::UsageUnknown)?;
        if self.total.is_some_and(|t| t != total) {
            return Err(Error::UsageUnknown);
        }
        Ok(total)
    }
}
/// Host catalog assertion: rates verified for this exact author model and tier,
/// in micro-USD per million tokens. Pricing requires verified inclusion semantics.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SvgAuthorPrice {
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub reasoning_in_output: bool,
}
impl SvgAuthorPrice {
    pub fn cost(&self, usage: &SvgUsage) -> Result<u64> {
        if usage.model.as_deref() != Some(self.model.as_str()) || !self.reasoning_in_output {
            return Err(Error::UsageUnknown);
        }
        usage.token_total()?;
        let input = usage.input.ok_or(Error::UsageUnknown)?;
        let output = usage.output.ok_or(Error::UsageUnknown)?;
        if usage.reasoning.is_some_and(|r| r > output) {
            return Err(Error::UsageUnknown);
        }
        let n = u128::from(input) * u128::from(self.input);
        let n = n
            .checked_add(u128::from(output) * u128::from(self.output))
            .ok_or(Error::UsageUnknown)?;
        u64::try_from(n.div_ceil(1_000_000)).map_err(|_| Error::UsageUnknown)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Rates in integer micro-USD per million tokens. The input side is a four-way partition of
/// disjoint lines, so each token is billed once and every rate is independent:
/// `input` (neither cached nor image), `cached` (cached, not image), `image_input` (image,
/// not cached) and `cached_image_input` (both). With `Usage` totals `i`, `cached`, `image`
/// and intersection `x`: ordinary = i - cached - image + x; cached-only = cached - x;
/// image-only = image - x; both = x. A counter that can change the bill must be known
/// or `cost` refuses with `UsageUnknown`.
pub struct Price {
    pub model: String,
    /// Input tokens that are neither cached nor image tokens.
    pub input: u64,
    /// Cached input tokens that are not image tokens.
    pub cached: u64,
    pub output: u64,
    pub reasoning: u64,
    /// Image input tokens that are not cached.
    pub image_input: u64,
    pub image_output: u64,
    /// Input tokens that are both cached and image tokens.
    pub cached_image_input: u64,
}
impl Price {
    /// Computes each side independently using the same partition rules as `cost`.
    /// A missing or ambiguous counter affects only its own side. This is audit
    /// evidence, not a charge or a replacement for the complete `cost` result.
    pub fn cost_breakdown(&self, usage: &Usage) -> Result<PriceEvidence> {
        if self.model != usage.model {
            return Err(Error::Invalid("price model mismatch".into()));
        }
        if let (Some(i), Some(o)) = (usage.input, usage.output) {
            usage.validate_subsets(i, o)?;
        } else {
            if let Some(i) = usage.input {
                usage.validate_subsets(i, usage.output.unwrap_or(u64::MAX))?;
            }
            if let Some(o) = usage.output {
                usage.validate_subsets(usage.input.unwrap_or(u64::MAX), o)?;
            }
        }
        let mut input = usage.clone();
        input.output = Some(0);
        input.reasoning = Some(0);
        input.image_output = Some(0);
        let mut output = usage.clone();
        output.input = Some(0);
        output.cached = Some(0);
        output.image_input = Some(0);
        output.cached_image_input = Some(0);
        Ok(PriceEvidence {
            price: self.clone(),
            input_micro_usd: usage.input.and_then(|_| self.cost(&input).ok()),
            output_micro_usd: usage.output.and_then(|_| self.cost(&output).ok()),
        })
    }
    /// Conservative reservation for an input upper bound and output allowance.
    /// Host-supplied category rates are integer micro-USD per million tokens.
    /// Rounds up once for this bill; overflow rejects the reservation.
    pub fn estimate(&self, input_upper_bound: u64, output_allowance: u64) -> Result<Amount> {
        let (input_rate, output_rate) = self.maximum_rates();
        let numerator = (u128::from(input_upper_bound) * u128::from(input_rate))
            .checked_add(u128::from(output_allowance) * u128::from(output_rate))
            .ok_or(Error::ReservationDenied)?;
        Ok(Amount::new(
            input_upper_bound
                .checked_add(output_allowance)
                .ok_or(Error::ReservationDenied)?,
            u64::try_from(numerator.div_ceil(1_000_000)).map_err(|_| Error::ReservationDenied)?,
        ))
    }
    fn maximum_rates(&self) -> (u64, u64) {
        (
            self.input
                .max(self.cached)
                .max(self.image_input)
                .max(self.cached_image_input),
            self.output.max(self.reasoning).max(self.image_output),
        )
    }
    /// Integer micro-USD per million tokens, rounded up once per billed attempt.
    pub fn cost(&self, u: &Usage) -> Result<u64> {
        let i = u.input.ok_or(Error::UsageUnknown)?;
        let o = u.output.ok_or(Error::UsageUnknown)?;
        u.validate_subsets(i, o)?;
        // A missing category may be ignored only when it cannot affect price.
        let c = known_subset(
            u.cached,
            i,
            self.cached != self.input
                || (u.image_input != Some(0) && self.cached_image_input != self.image_input),
        )?
        .max(u.cached_image_input.unwrap_or(0));
        let image = known_subset(
            u.image_input,
            i,
            self.image_input != self.input
                || (u.cached != Some(0) && self.cached_image_input != self.cached),
        )?
        .max(u.cached_image_input.unwrap_or(0));
        let cross = known_subset(
            u.cached_image_input,
            c.min(image),
            i128::from(self.input) + i128::from(self.cached_image_input)
                != i128::from(self.cached) + i128::from(self.image_input),
        )?;
        let cross = if u.cached_image_input.is_none() {
            cross.max(c.saturating_sub(i - image))
        } else {
            cross
        };
        let mut r = known_subset(u.reasoning, o, self.reasoning != self.output)?;
        let mut image_out = known_subset(u.image_output, o, self.image_output != self.output)?;
        // Reasoning and image output are each a subset of output, but providers do
        // not promise they are disjoint and no overlap count is reported. A
        // category billed at the plain output rate cannot change the cost, so it
        // is folded into ordinary output; if both carry their own rate and both
        // are nonzero, the overlap decides the price and settlement is refused.
        if self.reasoning == self.output {
            r = 0;
        }
        if self.image_output == self.output {
            image_out = 0;
        }
        if r > 0 && image_out > 0 {
            return Err(Error::UsageUnknown);
        }
        // Inclusion/exclusion can have a signed coefficient; all final lines are nonnegative.
        let ordinary = i
            .checked_sub(c)
            .and_then(|n| n.checked_sub(image.saturating_sub(cross)))
            .ok_or_else(|| Error::Invalid("usage intersection".into()))?;
        let n = [
            (ordinary, self.input),
            (c - cross, self.cached),
            (image - cross, self.image_input),
            (cross, self.cached_image_input),
            (o - r - image_out, self.output),
            (r, self.reasoning),
            (image_out, self.image_output),
        ]
        .into_iter()
        .try_fold(0u128, |total, (tokens, rate)| {
            total
                .checked_add(u128::from(tokens) * u128::from(rate))
                .ok_or(Error::ReservationDenied)
        })?;
        u64::try_from(n.div_ceil(1_000_000)).map_err(|_| Error::ReservationDenied)
    }
}
fn known_subset(value: Option<u64>, total: u64, required: bool) -> Result<u64> {
    value
        .or_else(|| (total == 0 || !required).then_some(0))
        .ok_or(Error::UsageUnknown)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelLimits {
    pub model: String,
    pub context: u64,
    pub max_output: u64,
    /// Technical minimum accepted max-output request parameter, not a guarantee
    /// of generated tokens. The host quality floor belongs to ReservationPolicy.
    pub min_max_output_tokens: u64,
    pub price: Option<Price>,
    /// Per-invocation timeout in milliseconds.
    pub timeout: u64,
}
impl ModelLimits {
    fn validated_price(&self) -> Result<&Price> {
        if self.context == 0
            || self.min_max_output_tokens == 0
            || self.min_max_output_tokens > self.max_output
            || self.max_output > self.context
        {
            return Err(Error::Invalid("model request bounds".into()));
        }
        let p = self.price.as_ref().ok_or(Error::PriceNotConfigured)?;
        if p.model != self.model {
            return Err(Error::PriceNotConfigured);
        }
        Ok(p)
    }
    /// Quote an explicit request without shrinking. Input is a host-supplied upper
    /// bound; output is a reserved allowance, not a promised generation length.
    pub fn estimate(&self, input_upper_bound: u64, output_allowance: u64) -> Result<Amount> {
        let p = self.validated_price()?;
        if output_allowance < self.min_max_output_tokens
            || output_allowance > self.max_output
            || input_upper_bound
                .checked_add(output_allowance)
                .is_none_or(|n| n > self.context)
        {
            return Err(Error::ReservationDenied);
        }
        p.estimate(input_upper_bound, output_allowance)
    }
    pub fn cap(&self, input: u64, remaining: Amount) -> Result<(u64, Amount)> {
        let p = self.validated_price()?;
        let (input_rate, output_rate) = p.maximum_rates();
        let input_cost = u128::from(input) * u128::from(input_rate);
        let money = (u128::from(remaining.micro_usd) * 1_000_000)
            .checked_sub(input_cost)
            .ok_or(Error::ReservationDenied)?;
        let money_cap = if output_rate == 0 {
            u64::MAX
        } else {
            u64::try_from(money / u128::from(output_rate)).unwrap_or(u64::MAX)
        };
        let cap = self
            .max_output
            .min(self.context.saturating_sub(input))
            .min(remaining.tokens.saturating_sub(input))
            .min(money_cap);
        if cap == 0 || cap < self.min_max_output_tokens {
            return Err(Error::ReservationDenied);
        }
        Ok((cap, self.estimate(input, cap)?))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RunHandle {
    pub tenant: String,
    pub id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSpec {
    pub tenant: String,
    pub principal: String,
    pub request_key: String,
    pub account: String,
    pub parent: Option<RunHandle>,
    pub session: Option<String>,
    pub context_revision: u64,
    pub basis_revision: u64,
    pub config_version: String,
    pub input: String,
    pub images: Vec<crate::images::ImageReference>,
    pub purpose: String,
    /// Absolute Run deadline in Unix milliseconds (Clock epoch); `None` is unbounded.
    pub deadline: Option<u64>,
    pub trigger: Trigger,
    pub validator: String,
    pub validator_version: String,
    pub budget: ReservationPolicy,
    pub scheduling: SchedulingPolicy,
    pub continuation: ContinuationPolicy,
    pub compaction: Option<CompactionPolicy>,
    #[serde(default)]
    pub completion: CompletionPolicy,
    #[serde(default)]
    pub transcript: TranscriptPolicy,
    /// Ordered phase configuration. An empty list uses a single conversation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<PhaseConfig>,
    /// Maximum consecutive reads of already covered ranges, excluding recovery reads.
    #[serde(default)]
    pub max_repeated_reads: Option<std::num::NonZeroU64>,
}
impl RunSpec {
    pub fn new(tenant: &str, key: &str, account: String, input: &str) -> Self {
        Self {
            tenant: tenant.into(),
            principal: "host".into(),
            request_key: key.into(),
            account,
            parent: None,
            session: None,
            context_revision: 0,
            basis_revision: 0,
            config_version: "1".into(),
            input: input.into(),
            images: vec![],
            purpose: "judgment".into(),
            deadline: None,
            trigger: Trigger::Api,
            validator: "host".into(),
            validator_version: "1".into(),
            budget: ReservationPolicy::default(),
            scheduling: SchedulingPolicy::default(),
            continuation: ContinuationPolicy::default(),
            compaction: None,
            completion: CompletionPolicy::default(),
            transcript: TranscriptPolicy::default(),
            phases: vec![],
            max_repeated_reads: None,
        }
    }
}
/// Successful token-limited responses start new, separately reserved turns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationPolicy {
    pub instruction: String,
    pub max_continuations: Option<u64>,
}
impl Default for ContinuationPolicy {
    fn default() -> Self {
        Self {
            instruction: "Continue from the previous response and complete the task.".into(),
            max_continuations: None,
        }
    }
}
/// Compact before a new model turn when context headroom falls below this value.
/// The host selects the summary model through ModelPort::compaction_limits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionPolicy {
    pub min_remaining_tokens: u64,
    /// Preserve typed observations and unresolved diagnostics in the next request.
    #[serde(default)]
    pub preserve_progress: bool,
    /// Maximum completed compactions without new typed progress.
    #[serde(default)]
    pub max_without_progress: Option<std::num::NonZeroU64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionPolicy {
    pub mode: CompletionMode,
    /// Name of an optional trusted tool returning ValidationFeedback.
    pub validation_tool: Option<String>,
    pub output_schema: Option<Value>,
    /// Request the configured output schema on supported tool-free model requests.
    pub provider_format: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionMode {
    #[default]
    FinalSubmission,
    ValidatedAck,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptPolicy {
    #[default]
    Retain,
    PurgeOnSuccess,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairTarget {
    OutputFormat,
    Workspace,
}

/// Result contract of the configured validation tool. The tool
/// must capture the same state version used to evaluate its receipt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidationFeedback {
    pub candidate: Value,
    pub receipt: ValidationReceipt,
    pub basis: String,
    pub repair_target: RepairTarget,
}

/// Evidence produced by a trusted adapter for a successfully applied operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolProgress {
    Read {
        namespace: crate::data::Namespace,
        path: String,
        revision: u64,
        start: u64,
        end: u64,
    },
    Write {
        namespace: crate::data::Namespace,
        path: String,
        revision: Option<u64>,
        hash: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trigger {
    Api,
    Event,
    Schedule,
    CaseAppend,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Terminal {
    Succeeded,
    Failed(String),
    Cancelled(String),
    DeadlineReached,
    BudgetReached(String),
    OutcomeUnknown(String),
    Stalled(ProgressStop),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProgressStop {
    Validation { codes: Vec<String> },
    RepeatedRead,
    Compaction,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunState {
    Queued,
    Running,
    Waiting(String),
    Finalizing,
    Terminal(Terminal),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Progress {
    NotRequired,
    Pending,
    Confirmed,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Delivery {
    None,
    Accepted(String),
    Confirmed(String),
    Superseded(String),
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub run: RunHandle,
    pub worker: String,
    pub generation: u64,
    /// Absolute lease expiry in Unix milliseconds: `claim`/`renew` set it to
    /// `now + lease` with both operands in milliseconds.
    pub lease_until: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub text: String,
    pub call_id: Option<String>,
    pub tool: Option<String>,
    pub data: Value,
}
impl Message {
    pub fn text(role: &str, text: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            text: text.into(),
            call_id: None,
            tool: None,
            data: Value::Null,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub schema: u32,
    pub revision: u64,
    pub payload: Value,
    pub messages: Vec<Message>,
    pub cursor: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub handle: RunHandle,
    pub spec: RunSpec,
    pub schema: u32,
    pub revision: u64,
    pub sequence: u64,
    pub owner: Option<Claim>,
    pub generation: u64,
    pub state: RunState,
    pub checkpoint: Option<Checkpoint>,
    pub cancel: Option<String>,
    pub stop: Progress,
    pub cleanup: Progress,
    pub settlement: Progress,
    pub delivery: Delivery,
    pub output: Option<Value>,
    pub receipt: Option<ValidationReceipt>,
    pub completion: Option<String>,
    #[serde(default)]
    pub phase: Option<PhaseState>,
}
impl RunRecord {
    /// State identity supplied by the trusted validator before an ACK turn.
    /// A host finalizer can compare it with the version it is about to adopt.
    pub fn validated_ack_basis(&self) -> Option<&str> {
        self.checkpoint.as_ref()?.payload["validated_ack"]["basis"].as_str()
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub tenant: String,
    pub parent: Option<String>,
    pub limit: Amount,
    pub reserved: Amount,
    pub used: Amount,
    pub unknown: u64,
    pub exhausted: bool,
    pub warnings: [u8; 2],
    pub policy: AccountPolicy,
    pub quantity_reserved: BTreeMap<String, u64>,
    pub quantity_used: BTreeMap<String, u64>,
    pub quantity_warnings: BTreeMap<String, u8>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum OperationKind {
    Run,
    Model,
    Tool,
    Compaction,
    ChildRun,
    Retry,
    Sandbox,
}
/// Opaque adapter proof, pinned atomically with the operation intent and reservation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryProof {
    pub bytes: Vec<u8>,
    pub generation: String,
    /// Absolute Clock timestamp in Unix milliseconds. A later preparation must never extend an old reservation.
    pub expires_at: u64,
}
impl RecoveryProof {
    pub fn validate(&self, now: u64) -> Result<()> {
        if self.bytes.is_empty()
            || self.bytes.len() > 65_536
            || self.generation.is_empty()
            || self.generation.len() > 256
            || self.expires_at <= now
        {
            return Err(Error::Invalid("recovery proof bounds/expiry".into()));
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct PreparationContext {
    pub claim: Claim,
    pub principal: String,
    pub key: String,
    pub attempt: u32,
    pub target: String,
}
/// Physical sends reported by an adapter; distinct from logical usage_observations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalSendObservation {
    pub sequence: u32,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceCompleteness {
    Complete,
    Truncated,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalSendEvidence {
    pub observations: Vec<PhysicalSendObservation>,
    pub completeness: EvidenceCompleteness,
}
impl PhysicalSendEvidence {
    pub fn validate(&self) -> Result<()> {
        if self.observations.len() > 256
            || self
                .observations
                .iter()
                .map(|o| o.bytes.len())
                .sum::<usize>()
                > 65_536
            || self
                .observations
                .windows(2)
                .any(|p| p[0].sequence >= p[1].sequence)
        {
            return Err(Error::Invalid("physical send evidence bounds/order".into()));
        }
        Ok(())
    }
}
/// A single billable extension of a previously settled provider lease.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceExtension {
    pub source_operation: String,
    pub resource_id: String,
    pub previous_expiry_ms: u64,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DispatchRequest {
    pub recovery_proof: Option<RecoveryProof>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension: Option<ResourceExtension>,
    pub key: String,
    pub kind: OperationKind,
    pub target: String,
    pub version: String,
    pub account_binding: String,
    pub input: Value,
    pub reserve: Amount,
    pub quantities: BTreeMap<String, u64>,
    pub price: Option<Price>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<Box<crate::models::PricingSnapshot>>,
    pub scope: Option<String>,
    /// Absolute Unix milliseconds until which recovery may replay or look up this operation.
    pub recovery_until: Option<u64>,
    pub attempt: u32,
    /// Operation timeout in milliseconds, added to `Clock::now()` to form the
    /// operation deadline; `None` leaves only the Run deadline.
    pub timeout: Option<u64>,
}
impl DispatchRequest {
    /// Prepared operations replay against the pinned proof generation. Tool scope
    /// remains the independently pinned descriptor digest.
    pub fn replay_scope(&self) -> Option<&str> {
        self.recovery_proof
            .as_ref()
            .map(|proof| proof.generation.as_str())
            .or(self.scope.as_deref())
    }
    pub fn new(
        key: &str,
        kind: OperationKind,
        target: &str,
        input: Value,
        reserve: Amount,
    ) -> Self {
        Self {
            recovery_proof: None,
            extension: None,
            key: key.into(),
            kind,
            target: target.into(),
            version: "1".into(),
            account_binding: "host".into(),
            input,
            reserve,
            quantities: BTreeMap::new(),
            price: None,
            pricing: None,
            scope: None,
            recovery_until: None,
            attempt: 1,
            timeout: Some(30_000),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationState {
    Intent,
    Sent,
    Accepted,
    Applied,
    Failed,
    Unknown,
}
/// Sanitized provider failure evidence. Values originate only from a bounded allowlist;
/// hosts should not put raw provider messages, headers, URLs or response bodies here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFailurePhase {
    PreSend,
    Send,
    ResponseRead,
    ResponseByteLimit,
    ResponseDecode,
    HttpError,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSendCertainty {
    DefinitelyNotSent,
    Sent,
    Unknown,
}
/// One provider-supplied identifier. Invalid or oversized values are omitted,
/// never truncated into a potentially misleading identifier.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SafeProviderField(String);
impl TryFrom<String> for SafeProviderField {
    type Error = &'static str;
    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'[' | b']'))
        {
            return Err("unsafe provider diagnostic field");
        }
        Ok(Self(value))
    }
}
impl From<SafeProviderField> for String {
    fn from(value: SafeProviderField) -> Self {
        value.0
    }
}
impl std::ops::Deref for SafeProviderField {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDiagnostic {
    pub phase: ProviderFailurePhase,
    pub send_certainty: ProviderSendCertainty,
    /// Actual transport status, distinct from any effective classification status.
    pub http_status: Option<u16>,
    pub provider_type: Option<SafeProviderField>,
    pub provider_code: Option<SafeProviderField>,
    pub provider_param: Option<SafeProviderField>,
    /// A validated provider request ID from the response, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<SafeProviderField>,
}
/// Evidence available when an operation's effect cannot be determined.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AmbiguityEvidence {
    Provider(ProviderDiagnostic),
    InvocationStarted,
    DispatchRecorded,
    ReceiptWriteAttempted,
    ReceiptObserved,
}

/// Why the effect is unresolved. Usage has its own classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AmbiguityReason {
    TransportAfterPossibleWrite,
    ResponseLost,
    ResponseDecode,
    ResponseByteLimit,
    ResponseIdentityUnverified,
    InvocationInterrupted,
    ReceiptCommitUnconfirmed,
    RecoveryUnconfirmed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AmbiguousOutcome {
    evidence: AmbiguityEvidence,
    reason: AmbiguityReason,
}

impl AmbiguousOutcome {
    pub fn evidence(&self) -> &AmbiguityEvidence {
        &self.evidence
    }

    pub fn reason(&self) -> AmbiguityReason {
        self.reason
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NonBillableReason {
    NotSent,
    RetryableNotSent,
    RejectedBeforeExecution,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UsageIssue {
    Missing,
    Incomplete,
    Invalid,
    PricingUnavailable,
    OutcomeUnresolved,
    SettlementUnverified,
    ArithmeticOverflow,
    ModelBindingMismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageOutcome<'a> {
    Measured(&'a Usage),
    NonBillable(NonBillableReason),
    Pending,
    Unresolved {
        reason: UsageIssue,
        reported: Option<&'a Usage>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ExternalResult {
    Applied {
        physical_evidence: Option<PhysicalSendEvidence>,
        output: Value,
        usage: Usage,
        external_id: Option<String>,
    },
    Accepted {
        external_id: String,
    },
    Failed {
        physical_evidence: Option<PhysicalSendEvidence>,
        code: String,
        usage: Usage,
        /// Minimum delay before the next attempt, in milliseconds.
        retry_after: Option<u64>,
        /// Opt in to a new model turn after a known, safely correctable failure.
        /// Both code and hint must be safe to disclose; never include payloads,
        /// secrets, storage/permission errors or uncertain side effects.
        /// None is fatal. Automatic retry_after takes precedence when present.
        repair_hint: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_diagnostic: Option<ProviderDiagnostic>,
    },
    Ambiguous(AmbiguousOutcome),
}
impl ExternalResult {
    pub fn ambiguous(evidence: AmbiguityEvidence, reason: AmbiguityReason) -> Self {
        Self::Ambiguous(AmbiguousOutcome { evidence, reason })
    }

    pub fn ambiguity(&self) -> Option<&AmbiguousOutcome> {
        match self {
            Self::Ambiguous(outcome) => Some(outcome),
            _ => None,
        }
    }

    #[doc(hidden)]
    pub fn clear_ambiguity_diagnostic(&mut self) {
        if let Self::Ambiguous(outcome) = self
            && let AmbiguityEvidence::Provider(diagnostic) = &mut outcome.evidence
        {
            diagnostic.provider_type = None;
            diagnostic.provider_code = None;
            diagnostic.provider_param = None;
            diagnostic.request_id = None;
        }
    }

    pub fn usage_outcome(&self) -> UsageOutcome<'_> {
        match self {
            Self::Accepted { .. } => UsageOutcome::Pending,
            Self::Ambiguous(_) => UsageOutcome::Unresolved {
                reason: UsageIssue::OutcomeUnresolved,
                reported: None,
            },
            Self::Applied { usage, .. } | Self::Failed { usage, .. } => usage.outcome(),
        }
    }

    pub fn retryable_not_sent() -> Self {
        Self::Failed {
            physical_evidence: None,
            code: "REQUEST_NOT_SENT".into(),
            usage: Usage::non_billable(NonBillableReason::RetryableNotSent),
            retry_after: None,
            repair_hint: None,
            provider_diagnostic: None,
        }
    }

    pub fn provider_diagnostic(&self) -> Option<&ProviderDiagnostic> {
        match self {
            Self::Failed {
                provider_diagnostic,
                ..
            } => provider_diagnostic.as_ref(),
            Self::Ambiguous(outcome) => match outcome.evidence() {
                AmbiguityEvidence::Provider(diagnostic) => Some(diagnostic),
                _ => None,
            },
            _ => None,
        }
    }
    pub fn validate_evidence(&self) -> Result<()> {
        match self {
            Self::Applied {
                physical_evidence: Some(e),
                ..
            }
            | Self::Failed {
                physical_evidence: Some(e),
                ..
            } => e.validate(),
            _ => Ok(()),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub run: RunHandle,
    /// Retired by an intermediate boundary. Only accounting evidence remains.
    #[serde(default)]
    pub retired: bool,
    pub request: DispatchRequest,
    pub state: OperationState,
    pub result: Option<ExternalResult>,
    pub settled: bool,
    pub unknown_usage: bool,
    pub send_generation: Option<u64>,
    pub send_count: u32,
    pub replay_proof_generation: Option<u64>,
    /// Unix milliseconds (Clock epoch).
    pub created_at: u64,
    /// Unix milliseconds (Clock epoch).
    pub finished_at: Option<u64>,
    pub usage_observations: Vec<Usage>,
    pub unsettled_result: Option<ExternalResult>,
    /// Most recent lookup failure, recorded separately from the invocation receipt.
    #[serde(default)]
    pub lookup_failure: Option<LookupFailure>,
    /// Present only when the owner settled this operation through
    /// [`OperationStore::settle_recovered`]; keeps the old usage, the price basis and
    /// the new usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<Box<SettlementRecord>>,
}
impl Operation {
    /// A reservation with no dispatch, replay or result evidence.
    pub fn is_unsent_intent(&self) -> bool {
        self.state == OperationState::Intent
            && self.send_count == 0
            && self.send_generation.is_none()
            && self.replay_proof_generation.is_none()
            && self.result.is_none()
            && self.unsettled_result.is_none()
            && self.usage_observations.is_empty()
            && !self.settled
            && !self.unknown_usage
    }
    /// A cancelled model or compaction intent needs no adapter cleanup.
    pub fn abortable_on_cancel(&self) -> bool {
        matches!(
            self.request.kind,
            OperationKind::Model | OperationKind::Compaction
        ) && self.is_unsent_intent()
    }
    /// Usage evidence in the context of the durable operation and its settlement.
    pub fn usage_outcome(&self) -> UsageOutcome<'_> {
        let Some(result) = self.unsettled_result.as_ref().or(self.result.as_ref()) else {
            return UsageOutcome::Pending;
        };
        match result.usage_outcome() {
            UsageOutcome::Measured(usage) if !self.settled => UsageOutcome::Unresolved {
                reason: UsageIssue::SettlementUnverified,
                reported: Some(usage),
            },
            outcome => outcome,
        }
    }

    /// Read the diagnostic retained alongside the result and settlement evidence.
    pub fn provider_diagnostic(&self) -> Option<&ProviderDiagnostic> {
        self.unsettled_result
            .as_ref()
            .and_then(ExternalResult::provider_diagnostic)
            .or_else(|| {
                self.result
                    .as_ref()
                    .and_then(ExternalResult::provider_diagnostic)
            })
    }
    /// Only a settled, explicitly correctable tool failure whose protocol answer
    /// was checkpointed may cease blocking successful completion.
    pub fn model_repair_acknowledged(&self, run: &RunRecord) -> bool {
        self.settled
            && self.run == run.handle
            && self.state == OperationState::Failed
            && self.request.kind == OperationKind::Tool
            && matches!(
                &self.result,
                Some(ExternalResult::Failed {
                    retry_after: None,
                    repair_hint: Some(_),
                    ..
                }) | Some(ExternalResult::Failed {
                    usage: Usage {
                        non_billable: Some(NonBillableReason::RetryableNotSent),
                        ..
                    },
                    retry_after: None,
                    ..
                })
            )
            && run
                .checkpoint
                .as_ref()
                .is_some_and(|cp| cp.payload["tool_repair"]["operations"][&self.id] == true)
    }
    /// The child receipt is a Run identity, never this operation's identity.
    pub fn child_handle(&self) -> Result<RunHandle> {
        if self.request.kind != OperationKind::ChildRun {
            return Err(Error::Conflict);
        }
        let handle = match &self.result {
            Some(ExternalResult::Accepted { external_id }) => RunHandle {
                tenant: self.run.tenant.clone(),
                id: external_id.clone(),
            },
            Some(ExternalResult::Applied { output, .. }) if self.settled => {
                serde_json::from_value(output.clone()).map_err(|_| Error::Conflict)?
            }
            _ => return Err(Error::Conflict),
        };
        if handle.tenant != self.run.tenant || handle.id == self.id {
            return Err(Error::Conflict);
        }
        Ok(handle)
    }
    /// A result persisted by a fenced observer, sufficient for this attempt's
    /// pinned price and quantity contract. Late unfenced observations are excluded.
    pub fn saved_settlement_result(&self) -> Result<Option<&ExternalResult>> {
        let Some(result) = self.unsettled_result.as_ref() else {
            return Ok(None);
        };
        result.validate_evidence()?;
        // This is a sufficiency probe. Malformed observations remain evidence,
        // but must not prevent lookup from supplying corrected measurements.
        // record still validates every new result before changing balances.
        Ok((self
            .request
            .settlement_result(result)
            .is_ok_and(|amount| amount.is_some())
            && self.settlement_allowance(result, u64::MAX).is_ok()
            && self.validate_result_send().is_ok())
        .then_some(result))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LookupFailure {
    PermissionDenied,
    Rejected(u16),
    Configuration,
    Transport,
    ResponseRead,
    ResponseDecode,
    ResponseByteLimit,
    IdentityMismatch,
}

// Keep lookup results inline: callers forward the receipt directly to atomic settlement.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Lookup {
    Found(ExternalResult),
    Pending,
    Unknown,
    NotFound { scope: String },
    Unavailable(LookupFailure),
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub version: String,
    pub account: String,
    pub permission: String,
    pub capability: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub side_effect: bool,
    /// Operation timeout in milliseconds.
    pub timeout: u64,
    pub max_output_bytes: usize,
    pub reserve: Amount,
    pub quantities: BTreeMap<String, u64>,
    pub conflict_scope: Option<String>,
    pub retry: RetryPolicy,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    /// Minimum wait before the next attempt, in milliseconds.
    pub backoff: u64,
    /// Maximum time since the first attempt, in milliseconds.
    pub max_elapsed: u64,
}
impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            backoff: 1_000,
            max_elapsed: 60_000,
        }
    }
}
/// Authorization bound to a stored attempt and its pinned price.
#[derive(Clone, Debug)]
pub struct ManagedSend {
    claim: Claim,
    operation: String,
    attempt: u32,
    pricing: Box<crate::models::PricingSnapshot>,
    reserve: Amount,
    stage: std::sync::Arc<std::sync::atomic::AtomicU8>,
    poll_id: Option<String>,
}

impl ManagedSend {
    const UNUSED: u8 = 0;
    const UNTRACKED_SEND: u8 = 1;
    const AWAITING_BODY: u8 = 2;
    const BODY_RELEASED: u8 = 3;
    const REVOKED: u8 = 4;

    /// Revoke further use if the adapter has not released its request body.
    pub fn revoke_if_not_sent(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.stage
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |stage| {
                matches!(stage, Self::UNUSED | Self::AWAITING_BODY).then_some(Self::REVOKED)
            })
            .is_ok()
    }

    /// Release a required request body to the HTTP transport, unless revoked.
    /// Once released, cancellation cannot prove the request was never written.
    pub fn begin_body(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.stage
            .compare_exchange(
                Self::AWAITING_BODY,
                Self::BODY_RELEASED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn new(claim: &Claim, op: &Operation) -> Option<Self> {
        Some(Self {
            claim: claim.clone(),
            operation: op.id.clone(),
            attempt: op.request.attempt,
            pricing: op.request.pricing.clone()?,
            reserve: op.request.reserve,
            stage: Default::default(),
            poll_id: None,
        })
    }

    pub(crate) fn for_lookup(claim: &Claim, op: &Operation) -> Option<Self> {
        if op.settled {
            return None;
        }
        let Some(ExternalResult::Accepted { external_id }) = &op.result else {
            return None;
        };
        let mut grant = Self::new(claim, op)?;
        grant.poll_id = Some(external_id.clone());
        Some(grant)
    }

    fn matches(&self, context: &ToolContext, pricing: &crate::models::PricingSnapshot) -> bool {
        self.claim == context.claim
            && self.operation == context.operation
            && self.attempt == context.attempt
            && self.pricing.as_ref() == pricing
    }

    /// The persisted remote ID of an accepted operation on this binding.
    pub fn poll_id(
        &self,
        context: &ToolContext,
        pricing: &crate::models::PricingSnapshot,
    ) -> Option<&str> {
        self.poll_id
            .as_deref()
            .filter(|_| self.matches(context, pricing))
    }

    /// Consume the grant at the handoff of a prepared request for sending.
    pub fn authorize(
        &self,
        context: &ToolContext,
        pricing: &crate::models::PricingSnapshot,
        required: Amount,
    ) -> Result<()> {
        self.consume(context, pricing, required, Self::UNTRACKED_SEND)
    }

    /// Authorize an HTTP request whose required body calls `begin_body` before
    /// releasing any payload to the transport.
    pub fn authorize_tracked(
        &self,
        context: &ToolContext,
        pricing: &crate::models::PricingSnapshot,
        required: Amount,
    ) -> Result<()> {
        self.consume(context, pricing, required, Self::AWAITING_BODY)
    }

    /// Check the binding before doing local preparation, without consuming it.
    pub fn check(
        &self,
        context: &ToolContext,
        pricing: &crate::models::PricingSnapshot,
        required: Amount,
    ) -> Result<()> {
        use std::sync::atomic::Ordering;
        if !self.matches(context, pricing)
            || self.poll_id.is_some()
            || self.reserve.tokens < required.tokens
            || self.reserve.micro_usd < required.micro_usd
            || self.stage.load(Ordering::Acquire) != Self::UNUSED
        {
            return Err(Error::ReservationDenied);
        }
        Ok(())
    }

    fn consume(
        &self,
        context: &ToolContext,
        pricing: &crate::models::PricingSnapshot,
        required: Amount,
        stage: u8,
    ) -> Result<()> {
        use std::sync::atomic::Ordering;
        self.check(context, pricing, required)?;
        self.stage
            .compare_exchange(Self::UNUSED, stage, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::ReservationDenied)?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ToolContext {
    pub managed_send: Option<ManagedSend>,
    pub recovery_proof: Option<RecoveryProof>,
    pub claim: Claim,
    pub principal: String,
    pub operation: String,
    pub attempt: u32,
    /// Absolute operation/Run deadline in Unix milliseconds (Clock epoch).
    pub deadline: Option<u64>,
    pub permission: String,
    pub account_binding: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDescriptor>,
    pub max_output: u64,
    pub metadata: Value,
    pub images: Vec<crate::images::ImageReference>,
    pub attachments: Vec<crate::attachments::Attachment>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelReply {
    pub text: String,
    pub calls: Vec<ToolCall>,
    pub complete: bool,
    pub metadata: Value,
}
/// JSON parser coordinates: one-based line/byte column (column 0 for empty input),
/// zero-based byte offset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonPosition {
    pub line: usize,
    pub column: usize,
    pub byte_offset: usize,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Trusted host classification; never inferred from a code or path.
    pub repair: DiagnosticRepair,
    /// Content observed by the validator, not a global workspace generation.
    pub observation: Option<ArtifactObservation>,
    pub code: String,
    pub severity: String,
    /// Instance JSON Pointer (or business diagnostic location).
    pub path: String,
    /// Schema JSON Pointer for schema diagnostics.
    pub schema_path: Option<String>,
    pub artifact: Option<String>,
    pub file: Option<String>,
    pub field: Option<String>,
    /// Half-open byte span in the diagnostic target.
    pub span: Option<(u64, u64)>,
    pub position: Option<JsonPosition>,
    pub cause: String,
    pub hint: String,
}
/// One classification supplies repair owner, write permission and repairability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticRepair {
    Agent,
    Host,
    Operator,
    Unknown,
}
impl DiagnosticRepair {
    pub fn agent_can_write(self) -> bool {
        self == Self::Agent
    }
    pub fn repairable(self) -> bool {
        self.agent_can_write()
    }
}
/// The host binds a stable artifact identity to the bytes/revision it checked.
/// Supply a content digest when rewriting identical bytes advances storage revisions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactObservation {
    pub artifact: String,
    pub version: ArtifactVersion,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactVersion {
    ContentDigest(String),
    Revision(String),
}
/// Every blocking diagnostic must explicitly permit Agent repair.
pub fn diagnostics_agent_repairable(diagnostics: &[Diagnostic]) -> bool {
    diagnostics.iter().any(|d| d.severity == "error")
        && diagnostics.iter().all(|d| {
            matches!(d.severity.as_str(), "info" | "warning")
                || (d.severity == "error" && d.repair.repairable())
        })
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ValidationReceipt {
    pub hash: String,
    pub basis_revision: u64,
    pub config_version: String,
    pub validator: String,
    pub validator_version: String,
    pub passed: bool,
    /// Trusted validator classification, bound to the same candidate and version.
    pub repairable: bool,
    pub diagnostics: Vec<Diagnostic>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Completion {
    pub id: String,
    pub output: Value,
    pub receipt: ValidationReceipt,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    pub account: String,
    pub token_percent: Option<u8>,
    pub cost_percent: Option<u8>,
    pub quantity_percent: BTreeMap<String, u8>,
    pub used: Amount,
    pub limit: Amount,
}

/// Default capacity-queue wait, in milliseconds, for a Run without a deadline.
pub const CAPACITY_WAIT_MS: u64 = 60_000;

/// Host time source. Every time and duration carried as `u64` by this crate is in
/// **milliseconds**: absolute values (deadlines, lease expiry, retention and
/// recovery boundaries, `created_at`) are Unix milliseconds in the epoch this
/// clock reports, and durations (timeouts, leases, backoff, `max_elapsed`,
/// retention periods) are milliseconds added to such a value. Never mix in seconds.
/// Arithmetic on them is checked or saturating. Real-time sleeps use
/// `Duration::from_millis`. A persistent store shares one epoch across workers;
/// a fake clock may start at 0.
pub trait Clock: Send + Sync {
    /// Current time in Unix milliseconds.
    fn now(&self) -> u64;
    /// Completes once `now() >= deadline`; `deadline` is absolute Unix milliseconds.
    /// A deadline already in the past completes immediately.
    fn wait_until(&self, deadline: u64) -> impl std::future::Future<Output = ()> + Send;
}
pub trait Policy: Send + Sync {
    fn authorize(&self, run: &RunRecord, target: &str, permission: &str) -> Result<()>;
}
pub trait ModelPort: Send + Sync {
    fn supports_output_schema(&self) -> bool {
        false
    }
    /// Pure admission check for retained model inputs, without resolving payloads
    /// or sending inference. Return MODEL_ATTACHMENT_LIMIT for size/count
    /// failures so the driver can demote replayed media or return repairable tool feedback.
    fn check_input_attachments(
        &self,
        _attachments: &[crate::attachments::Attachment],
        _images: &[crate::images::ImageReference],
    ) -> Result<()> {
        Ok(())
    }
    /// A host port may route summary requests to a different configured model.
    /// prepare/invoke/lookup must route by the request's model, including recovery.
    fn compaction_limits(&self) -> ModelLimits {
        self.limits()
    }
    /// Fix attachment identity and prepare authorized local bytes before sizing
    /// and reservation. External transport resolution may wait until invoke.
    fn prepare_request(
        &self,
        _run: &RunRecord,
        request: ModelRequest,
    ) -> impl std::future::Future<Output = Result<PreparedModelRequest>> + Send {
        async move { PreparedModelRequest::text(request) }
    }
    /// Only declared adapters are prepared; ordinary local tools need no proof.
    fn requires_recovery_preparation(&self) -> bool {
        false
    }
    fn prepare_recovery(
        &self,
        _context: &PreparationContext,
        _input: &Value,
    ) -> impl std::future::Future<Output = Result<Option<RecoveryProof>>> + Send {
        async move { Ok(None) }
    }

    fn cancel(
        &self,
        _context: &ToolContext,
    ) -> impl std::future::Future<Output = Result<Progress>> + Send {
        async move { Ok(Progress::Unknown) }
    }
    fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy::default()
    }
    fn limits(&self) -> ModelLimits;
    fn pricing_snapshot(&self, _model: &str) -> Option<crate::models::PricingSnapshot> {
        None
    }
    fn input_upper_bound(&self, request: &PreparedModelRequest) -> Result<u64>;
    /// An independent input ceiling to consider when deciding whether to compact.
    /// Return None when only the combined context bound applies.
    fn compaction_input_limit(&self, _request: &PreparedModelRequest) -> Option<u64> {
        None
    }
    /// Recovery scope and retention in milliseconds; the driver pins
    /// `recovery_until = Clock::now() + retention` (checked).
    fn recovery(&self) -> Option<(String, u64)> {
        None
    }
    fn invoke(
        &self,
        context: &ToolContext,
        request: PreparedModelRequest,
    ) -> impl std::future::Future<Output = Result<ExternalResult>> + Send;
    fn lookup(
        &self,
        context: &ToolContext,
        request: PreparedModelRequest,
    ) -> impl std::future::Future<Output = Result<Lookup>> + Send;
}
pub trait Tool: Send + Sync {
    /// Derive observations from the pinned input and settled output. Pure and
    /// deterministic, so replay after a crash yields the same checkpoint.
    fn progress(&self, _input: &Value, _output: &Value) -> Result<Vec<ToolProgress>> {
        Ok(vec![])
    }
    fn pricing_snapshot(&self) -> Option<crate::models::PricingSnapshot> {
        None
    }
    /// Recheck live authorization for this exact model input before either a new
    /// invocation or replaying a settled result into model history. This hook
    /// must not acquire or expose the referenced content.
    ///
    /// Return `Error::InvalidToolInput` with a JSON Pointer and a model-safe
    /// hint for a correctable input to a new call. Runtime answers that call
    /// with `TOOL_INPUT_INVALID` before quoting or starting an attempt. On a
    /// saved attempt the same error denies replay. Use `PermissionDenied` for
    /// Run, principal, phase or grant failures that the model cannot correct.
    fn authorize_input(
        &self,
        _run: &RunRecord,
        _input: &Value,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async move { Ok(()) }
    }
    fn authorize_result(
        &self,
        _run: &RunRecord,
        _output: &Value,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        async move { Ok(()) }
    }
    /// Preflight the complete request and quote any floor above the descriptor's
    /// configured reserve. Runtime reserves the maximum of both. Return
    /// `InvalidToolInput` only for caller input that the Agent can correct;
    /// host configuration, permission, storage and other failures remain ordinary errors.
    fn quote(
        &self,
        _run: &RunRecord,
        _input: &Value,
    ) -> impl std::future::Future<Output = Result<Amount>> + Send {
        async move { Ok(Amount::default()) }
    }
    /// Resource admission class; execution tools reserve Sandbox slots.
    fn operation_kind(&self) -> OperationKind {
        OperationKind::Tool
    }
    /// Only declared adapters are prepared; ordinary local tools need no proof.
    fn requires_recovery_preparation(&self) -> bool {
        false
    }
    fn prepare_recovery(
        &self,
        _context: &PreparationContext,
        _input: &Value,
    ) -> impl std::future::Future<Output = Result<Option<RecoveryProof>>> + Send {
        async move { Ok(None) }
    }

    fn cancel(
        &self,
        _context: &ToolContext,
        _input: Value,
    ) -> impl std::future::Future<Output = Result<Progress>> + Send {
        async move { Ok(Progress::Unknown) }
    }
    fn descriptor(&self) -> ToolDescriptor;
    /// Return a receipt for a known effect or rejection. The runtime classifies
    /// `InvalidToolInput`, `PermissionDenied`, `NotFound`, `RequestByteLimit` and
    /// `Unsupported` as pre-effect errors. Use those variants only when the
    /// request has been rejected before execution.
    ///
    /// A transient pre-send fault can return `ExternalResult::retryable_not_sent`.
    /// An effect that may have happened requires `ExternalResult::ambiguous`.
    fn invoke(
        &self,
        context: &ToolContext,
        input: Value,
    ) -> impl std::future::Future<Output = Result<ExternalResult>> + Send;
    fn lookup(
        &self,
        context: &ToolContext,
        input: Value,
    ) -> impl std::future::Future<Output = Result<Lookup>> + Send;
}
/// Host supplies limits and business-specific diagnostic classification.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepairPolicy {
    pub max_repairs: Option<u64>,
    /// Maximum repair time in milliseconds from the first repair.
    pub max_elapsed: Option<u64>,
    /// Stop on this many occurrences of the same observed failure, including cycles.
    /// `None` disables this stop; budget, deadline and cancellation remain active.
    #[serde(deserialize_with = "deserialize_no_progress_limit")]
    pub max_no_progress: Option<std::num::NonZeroU64>,
}
fn deserialize_no_progress_limit<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<std::num::NonZeroU64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<std::num::NonZeroU64>::deserialize(deserializer)
}
impl Default for RepairPolicy {
    fn default() -> Self {
        Self {
            max_repairs: None,
            max_elapsed: None,
            max_no_progress: std::num::NonZeroU64::new(3),
        }
    }
}
pub trait Validator: Send + Sync {
    /// Return the current identity of all mutable state checked by an
    /// acknowledgement tool. Opt in explicitly when using ValidatedAck.
    fn completion_basis(&self, _run: &RunRecord) -> Result<String> {
        Err(Error::Unsupported("validated acknowledgement basis".into()))
    }
    /// Choose the model repair surface for a final candidate rejected by this validator.
    fn repair_target(&self, _receipt: &ValidationReceipt) -> RepairTarget {
        RepairTarget::Workspace
    }
    fn repair_policy(&self) -> RepairPolicy {
        RepairPolicy::default()
    }
    fn validate(&self, run: &RunRecord, candidate: &Value) -> Result<ValidationReceipt>;
}
pub trait Finalizer: Send + Sync {
    fn finalize(
        &self,
        run: &RunRecord,
        candidate: Value,
        receipt: ValidationReceipt,
    ) -> Result<Completion>;

    /// Erase phase-owned payloads outside the control store, close old grants,
    /// and return the authorized projection for the next conversation.
    /// Called after sealing. Retry by Run and phase identity until all backends
    /// acknowledge deletion. The operation must be idempotent.
    fn prepare_next_phase(&self, _run: &RunRecord, _seal: &PhaseSeal) -> Result<PhaseInput> {
        Err(Error::Unsupported("phase payload handoff".into()))
    }
}
pub trait SecretProvider: Send + Sync {
    fn with_secret<R: Send>(
        &self,
        context: &ToolContext,
        target: &str,
        use_secret: impl FnOnce(&[u8]) -> Result<R> + Send,
    ) -> impl std::future::Future<Output = Result<R>> + Send;
}
pub trait Observer {
    fn observe(&self, event: &Event);
}
#[derive(Clone, Debug)]
pub struct Event {
    pub run: RunHandle,
    pub operation: Option<String>,
    pub revision: u64,
    pub code: String,
}

pub trait RunStore: Send + Sync {
    /// Support for the atomic seal/scrub and fenced phase handoff.
    fn supports_phases(&self) -> bool {
        false
    }
    fn seal_phase(
        &self,
        _claim: &Claim,
        _expected_revision: u64,
        _output: Value,
        _receipt: ValidationReceipt,
        _now: u64,
    ) -> impl std::future::Future<Output = Result<RunRecord>> + Send {
        async { Err(Error::Unsupported("intermediate finalization".into())) }
    }
    fn open_phase(
        &self,
        _claim: &Claim,
        _expected_revision: u64,
        _input: PhaseInput,
        _now: u64,
    ) -> impl std::future::Future<Output = Result<RunRecord>> + Send {
        async { Err(Error::Unsupported("intermediate finalization".into())) }
    }
    /// True when successful finalization atomically clears the run's transcript
    /// and preserves account, attempt and completion metadata.
    fn supports_transcript_purge(&self) -> bool {
        false
    }
    /// Extend the current owner's lease without changing RunRecord::revision.
    /// The returned Claim and persisted owner.lease_until witness renewal;
    /// checkpoint CAS remains valid across same-generation heartbeats.
    /// Expired leases and mismatched worker/generation remain fenced out.
    /// `now` is Unix milliseconds and `lease` is a nonzero duration in
    /// milliseconds; the expiry only moves forward (`max(old, now + lease)`).
    fn renew(
        &self,
        claim: &Claim,
        now: u64,
        lease: u64,
    ) -> impl std::future::Future<Output = Result<Claim>> + Send;
    /// Strongly consistent lookup of the original request: an equal spec finds
    /// its run, a conflicting spec returns Conflict, and None proves absence.
    fn find_request(
        &self,
        spec: &RunSpec,
    ) -> impl std::future::Future<Output = Result<Option<RunHandle>>> + Send;
    /// Atomically admit a request or return the run for the same original spec;
    /// concurrent equal requests must never create more than one run. Children
    /// of phased Runs must be started with `admit_child`.
    fn start(&self, spec: RunSpec) -> impl std::future::Future<Output = Result<RunHandle>> + Send;
    /// Fence the parent and atomically persist a child, queue entry, lifetime slot,
    /// and its handle in the parent operation. Equal input is idempotent; any
    /// changed spec (including account/config) conflicts. For a phased parent the
    /// logical key and receipt are scoped to the current open phase. For an
    /// unphased parent existing receipts remain readable after cancellation or
    /// deadline. Ancestor Session reuse is rejected. No external send occurs.
    /// Trusted persistence boundary: callers must authorize through Runtime or
    /// host policy before entering the transaction; the store invokes no policy.
    fn admit_child(
        &self,
        claim: &Claim,
        operation_key: &str,
        spec: RunSpec,
        now: u64,
    ) -> impl std::future::Future<Output = Result<RunHandle>> + Send;
    fn get(&self, run: &RunHandle) -> impl std::future::Future<Output = Result<RunRecord>> + Send;
    /// `now` is Unix milliseconds and `lease` is a nonzero duration in
    /// milliseconds; the owner is valid while `now < lease_until`.
    fn claim(
        &self,
        run: &RunHandle,
        worker: &str,
        now: u64,
        lease: u64,
    ) -> impl std::future::Future<Output = Result<Claim>> + Send;
    fn check(
        &self,
        claim: &Claim,
        now: u64,
        new_work: bool,
    ) -> impl std::future::Future<Output = Result<RunRecord>> + Send;
    fn checkpoint(
        &self,
        claim: &Claim,
        expected: u64,
        checkpoint: Checkpoint,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn cancel(
        &self,
        run: &RunHandle,
        reason: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn terminate(
        &self,
        claim: &Claim,
        terminal: Terminal,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn finalize(
        &self,
        claim: &Claim,
        completion: Completion,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn wait(
        &self,
        claim: &Claim,
        reason: &str,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn cleanup(
        &self,
        claim: &Claim,
        stop: Progress,
        cleanup: Progress,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
}
pub trait BudgetAuthority: Send + Sync {
    fn create_account_with_policy(
        &self,
        tenant: &str,
        limit: Amount,
        parent: Option<String>,
        policy: AccountPolicy,
    ) -> impl std::future::Future<Output = Result<String>> + Send;
    /// Fenced, zero-charge finalization admission; does not authorize new I/O.
    fn finalization_allowed(
        &self,
        claim: &Claim,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn exhaust(
        &self,
        claim: &Claim,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn create_account(
        &self,
        tenant: &str,
        limit: Amount,
        parent: Option<String>,
    ) -> impl std::future::Future<Output = Result<String>> + Send;
    fn account(&self, id: &str) -> impl std::future::Future<Output = Result<Account>> + Send;
    fn remaining(
        &self,
        claim: &Claim,
        now: u64,
    ) -> impl std::future::Future<Output = Result<Amount>> + Send;
    fn warnings(
        &self,
        claim: &Claim,
        now: u64,
    ) -> impl std::future::Future<Output = Result<Vec<Warning>>> + Send;
}
pub trait OperationStore: Send + Sync {
    /// Persist a final adapter result under the current claim without settling.
    /// A complete saved result is immutable and can be recovered by a successor.
    /// Incomplete evidence retains reservations; stale observations never authorize
    /// settlement. After settlement, result is canonical and temporary copies clear.
    fn observe_result(
        &self,
        claim: &Claim,
        id: &str,
        result: ExternalResult,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    /// Record a failed lookup under the active claim, without changing the receipt.
    fn record_lookup_failure(
        &self,
        claim: &Claim,
        id: &str,
        failure: LookupFailure,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn proven_unsent(
        &self,
        claim: &Claim,
        id: &str,
        scope: &str,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    /// Atomically settle a cancelled, never-sent model or compaction intent,
    /// an unsent retry intent, or an expired, unsent resource extension intent. Requires the current claim and no result or usage evidence. Keeps
    /// a failed no-send audit record and releases only this attempt's reservation
    /// and slot; a sent or observed attempt can never be aborted this way.
    fn abort_unsent(
        &self,
        claim: &Claim,
        id: &str,
        now: u64,
    ) -> impl std::future::Future<Output = Result<ExternalResult>> + Send;
    /// Atomic reservation across all ancestors plus external intent; no I/O before
    /// success. ChildRun is rejected: internal children must use admit_child.
    /// A resource extension requires the matching settled lease and a single
    /// successor per source operation, checked in the admission transaction.
    fn begin(
        &self,
        claim: &Claim,
        request: DispatchRequest,
        now: u64,
    ) -> impl std::future::Future<Output = Result<Operation>> + Send;
    fn list_operations(
        &self,
        run: &RunHandle,
    ) -> impl std::future::Future<Output = Result<Vec<Operation>>> + Send;
    fn operation(
        &self,
        run: &RunHandle,
        key: &str,
    ) -> impl std::future::Future<Output = Result<Option<Operation>>> + Send;
    /// Every send, including a proven-not-sent replay, requires a fresh fence.
    /// A resource extension may only be sent before the previous lease expires.
    fn mark_sent(
        &self,
        claim: &Claim,
        id: &str,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    /// Owner recovery for an operation whose saved Applied/Failed result is complete but
    /// whose usage never settled. Idempotent for the same command key, digest and
    /// typed request; returns its immutable receipt. A new key cannot claim an
    /// already recovered effect. Makes no provider
    /// call and takes no Run claim. Releases the reservation and unknown mark only by
    /// settling the corrected amount, and keeps the old usage, price basis and new usage
    /// in `Operation.recovery`. Stored evidence that still cannot be settled returns
    /// `UsageUnknown` and changes nothing. Callers authorize the operator before calling.
    fn settle_recovered(
        &self,
        run: &RunHandle,
        id: &str,
        recovery: SettlementRecovery,
        command: OwnerCommand,
        now: u64,
    ) -> impl std::future::Future<Output = Result<OwnerReceipt>> + Send;
    /// Explicit owner action that lifts the operator hold set by `settle_recovered` (or by the
    /// operator-action wait). Settling never resumes a Run; only this does. Refused with
    /// `Conflict` for a stale revision or an operation still awaiting the operator,
    /// `Closed` for cancellation/terminal state, `NotHeld` without a hold,
    /// `OwnershipLost` for a live claim, `Deadline` at/after the deadline, or the
    /// account admission error. All checks and the immutable receipt are atomic.
    /// The Run becomes `Queued`, its expired owner is cleared, and a worker must claim it.
    /// A repeated command returns its original receipt before checking current state.
    fn resume_held(
        &self,
        run: &RunHandle,
        expected_revision: u64,
        command: OwnerCommand,
        now: u64,
    ) -> impl std::future::Future<Output = Result<OwnerReceipt>> + Send;
    /// Read-only historical lookup, independent of the Run's current state.
    /// Keys are scoped to the store (Postgres schema); hosts authorize access.
    fn owner_receipt(
        &self,
        key: &str,
    ) -> impl std::future::Future<Output = Result<Option<OwnerReceipt>>> + Send;
    /// Atomic result and settlement. Existing operations remain writable after cancellation.
    /// A certified allowance debit commits with a new settlement. A resource
    /// extension settles against its pinned resource and expiry.
    fn record(
        &self,
        claim: &Claim,
        id: &str,
        result: ExternalResult,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotRequest {
    pub key: String,
    pub tenant: String,
    pub run: String,
    pub kind: OperationKind,
}
#[derive(Clone, Debug)]
pub struct QueueEntry {
    pub key: String,
    pub tenant: String,
    pub scheduling: SchedulingPolicy,
    /// Absolute Unix milliseconds; at or after this time `dequeue` expires the
    /// entry with `QueueTimeout`. A Run without a deadline waits `CAPACITY_WAIT_MS`.
    pub deadline: u64,
}
#[derive(Clone, Debug)]
pub struct Capacity {
    pub global: BTreeMap<OperationKind, usize>,
    pub tenant: BTreeMap<OperationKind, usize>,
    pub run: BTreeMap<OperationKind, usize>,
    pub queue: usize,
    pub tenant_queue: usize,
}
impl Default for Capacity {
    fn default() -> Self {
        let limits = BTreeMap::from([
            (OperationKind::Run, 16),
            (OperationKind::Model, 8),
            (OperationKind::Tool, 8),
            (OperationKind::Sandbox, 2),
            (OperationKind::ChildRun, 4),
            (OperationKind::Retry, 8),
            (OperationKind::Compaction, 2),
        ]);
        Self {
            global: limits.clone(),
            tenant: limits.clone(),
            run: limits,
            queue: 64,
            tenant_queue: 16,
        }
    }
}
pub trait Scheduler: Send + Sync {
    fn acquire(&self, request: SlotRequest)
    -> impl std::future::Future<Output = Result<()>> + Send;
    fn release_slot(&self, key: &str) -> impl std::future::Future<Output = Result<()>> + Send;
    fn enqueue(&self, entry: QueueEntry) -> impl std::future::Future<Output = Result<()>> + Send;
    fn dequeue(
        &self,
        now: u64,
    ) -> impl std::future::Future<Output = Result<Option<QueueEntry>>> + Send;
}
pub trait ControlStore: RunStore + BudgetAuthority + OperationStore + Scheduler {}
impl<T: RunStore + BudgetAuthority + OperationStore + Scheduler> ControlStore for T {}

/// Content identity for structured decisions and validation receipts.
/// Object key order is not semantic and may change through durable JSON stores.
pub fn digest(value: &Value) -> String {
    use sha2::{Digest, Sha256};
    let mut value = value.clone();
    value.sort_all_objects();
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&value).expect("JSON value serializes"))
    )
}
